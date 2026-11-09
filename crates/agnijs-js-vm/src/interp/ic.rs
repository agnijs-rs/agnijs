//! Inline caches for `GetNamed` / `SetNamed` (ADR 0012, TASK-100.20), and
//! the dense-element fast paths of `GetKeyed` / `SetKeyed`.
//!
//! Each named access site has up to [`IC_ENTRIES`] entries in its block's
//! feedback ([`agnijs_js_compiler::Feedback`]); a site that sees more
//! receivers goes megamorphic for good. An entry records, for one kind of
//! receiver, what the lookup found: an own data property or accessor, a
//! data property or accessor on the prototype chain, the `length` of an
//! array or of a string primitive, or a property found from a string
//! primitive through `String.prototype`. A hit needs no key interning, no
//! hashing and no prototype walk; an accessor hit hands back the getter or
//! setter to call.
//!
//! Soundness rests on these facts of the object model:
//!
//! - A shape fixes the keys and attributes of every slot (changing
//!   attributes or deleting a property other than the last moves the
//!   object to dictionary mode, or to another shape), and shapes are never
//!   freed, so a shape id never comes to mean another layout. Whether a
//!   slot holds data or an accessor is read live and must match the entry.
//! - Dictionary-mode objects (shape 0, like an empty object) and objects
//!   with exotic named-property behaviour (proxies, typed arrays, module
//!   namespaces, mapped arguments) never take part, as receivers or on the
//!   chain. Arrays and String objects do, except for `length`, which is
//!   exotic on them.
//! - Objects a prototype entry looked through or into are *watched*. A
//!   change to a watched object's keys, attributes or prototype, or its
//!   death (after which its id can be reused), moves the heap's prototype
//!   epoch, and a prototype entry only hits in the epoch it was recorded
//!   in. Within an epoch, the receiver's shape (it lacks the key) and its
//!   prototype's id (the chain starts where it did) pin down the holder.
//!
//! Property values are always read from the live slot.

use crate::object::{Class, Elements, ObjectData};
use crate::realm::Intrinsic;
use crate::value::{ObjId, PropKey, PropSlot, Value};
use crate::vm::Vm;
use agnijs_js_compiler::bytecode::{CodeBlock, IcEntry, IcIdx, IcSite, IC_ENTRIES, NO_IC};

/// Hops a prototype entry may look through before the cache gives up.
const MAX_DEPTH: usize = 16;

/// A cache hit for a read.
#[derive(Clone, Copy, Debug)]
pub enum IcGet {
    /// The property's value.
    Value(Value),
    /// The getter to call with the receiver as `this`.
    Getter(Value),
}

/// Inline-cache counters (feature `ic-stats`).
#[derive(Default, Debug)]
pub struct IcStats {
    pub get_hits: std::cell::Cell<u64>,
    pub get_misses: std::cell::Cell<u64>,
    pub set_hits: std::cell::Cell<u64>,
    pub set_misses: std::cell::Cell<u64>,
    pub fills: std::cell::Cell<u64>,
    pub megamorphic: std::cell::Cell<u64>,
    pub keyed_hits: std::cell::Cell<u64>,
    pub keyed_misses: std::cell::Cell<u64>,
}

/// Count an inline-cache event (feature `ic-stats`; otherwise nothing).
macro_rules! ic_stat {
    ($vm:expr, $field:ident) => {{
        #[cfg(feature = "ic-stats")]
        {
            let c = &$vm.ic_stats.$field;
            c.set(c.get() + 1);
        }
    }};
}
pub(crate) use ic_stat;
use ic_stat as stat;

/// Objects whose `length` is not an ordinary property.
#[inline]
fn length_exotic(o: &ObjectData) -> bool {
    matches!(o.class, Class::Array(_) | Class::String(_))
}

/// A receiver the cache understands.
#[inline]
fn receiver_ok(o: &ObjectData) -> bool {
    o.dict.is_none()
        && !matches!(
            o.class,
            Class::Arguments(Some(_)) | Class::TypedArray(_) | Class::Namespace(_) | Class::Proxy(_)
        )
}

/// An object whose own properties the cache may look through or into on
/// a prototype chain, for a key that is (or is not) `length`.
#[inline]
fn chain_ok(o: &ObjectData, is_length: bool) -> bool {
    receiver_ok(o) && !(is_length && length_exotic(o))
}

/// Objects whose integer-keyed properties are their dense elements.
#[inline]
fn elements_ok(o: &ObjectData) -> bool {
    !matches!(
        o.class,
        Class::String(_)
            | Class::Arguments(Some(_))
            | Class::TypedArray(_)
            | Class::Namespace(_)
            | Class::Proxy(_)
    )
}

/// The array index a number names, if any.
#[inline]
fn index_of(n: f64) -> Option<usize> {
    let i = n as u32;
    // -0 names index 0, like +0; NaN, fractions, negatives and values out
    // of range fail the round trip. 2^32 - 1 is not an array index.
    if i as f64 == n && i != u32::MAX {
        Some(i as usize)
    } else {
        None
    }
}

#[inline]
fn read_slot(slot: Option<&PropSlot>, accessor: bool) -> Option<IcGet> {
    match (slot?, accessor) {
        (PropSlot::Data(v), false) => Some(IcGet::Value(*v)),
        (PropSlot::Accessor { get, .. }, true) => Some(if get.is_undefined() {
            IcGet::Value(Value::Undefined)
        } else {
            IcGet::Getter(*get)
        }),
        _ => None,
    }
}

impl Vm {
    #[inline]
    fn ic_site<'c>(&self, code: &'c CodeBlock, ic: IcIdx) -> Option<&'c IcSite> {
        if ic == NO_IC {
            return None;
        }
        code.feedback_if(self.feedback_owner)
            .map(|f| &f.ic[ic as usize])
    }

    /// The cached read of a named property of `recv` at site `ic`, if an
    /// entry applies. (Callers count hits and misses.)
    #[inline]
    pub(crate) fn ic_get(&self, code: &CodeBlock, ic: IcIdx, recv: Value) -> Option<IcGet> {
        let site = self.ic_site(code, ic)?;
        match recv {
            Value::Object(oid) => {
                let o = self.heap.obj(oid);
                if !receiver_ok(o) {
                    return None;
                }
                for c in &site.entries {
                    let e = c.get();
                    if e.flags & IcEntry::FLAG_LENGTH != 0 && length_exotic(o) {
                        continue;
                    }
                    match e.kind {
                        IcEntry::KIND_OWN_DATA | IcEntry::KIND_OWN_ACCESSOR => {
                            if o.shape.0 == e.shape {
                                return read_slot(
                                    o.slots.get(e.slot as usize),
                                    e.kind == IcEntry::KIND_OWN_ACCESSOR,
                                );
                            }
                        }
                        IcEntry::KIND_PROTO_DATA | IcEntry::KIND_PROTO_ACCESSOR => {
                            if o.shape.0 == e.shape
                                && o.proto.map(|p| p.0) == Some(e.proto)
                                && e.epoch == self.heap.proto_epoch
                            {
                                let h = self.heap.obj(ObjId(e.holder));
                                return read_slot(
                                    h.slots.get(e.slot as usize),
                                    e.kind == IcEntry::KIND_PROTO_ACCESSOR,
                                );
                            }
                        }
                        IcEntry::KIND_ARRAY_LENGTH => {
                            if let Class::Array(a) = &o.class {
                                return Some(IcGet::Value(Value::Number(a.length as f64)));
                            }
                        }
                        IcEntry::KIND_EMPTY | IcEntry::KIND_MEGAMORPHIC => return None,
                        _ => {}
                    }
                }
                None
            }
            Value::String(s) => {
                for c in &site.entries {
                    let e = c.get();
                    match e.kind {
                        IcEntry::KIND_STRING_LENGTH => {
                            return Some(IcGet::Value(Value::from_usize(self.heap.str_len(s))));
                        }
                        IcEntry::KIND_STRING_PROTO_DATA => {
                            if e.epoch == self.heap.proto_epoch
                                && self.intrinsic(Intrinsic::StringPrototype).0 == e.proto
                            {
                                let h = self.heap.obj(ObjId(e.holder));
                                return read_slot(h.slots.get(e.slot as usize), false);
                            }
                        }
                        IcEntry::KIND_EMPTY | IcEntry::KIND_MEGAMORPHIC => return None,
                        _ => {}
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// After a miss: record what a read of `key` on `recv` finds, when the
    /// cache can describe it.
    pub(crate) fn ic_fill_get(&mut self, code: &CodeBlock, ic: IcIdx, recv: Value, key: PropKey) {
        if ic == NO_IC || !matches!(key, PropKey::Str(_)) {
            return;
        }
        let Some(fb) = code.feedback(self.feedback_owner) else {
            return;
        };
        let site = &fb.ic[ic as usize];
        if site.entries[0].get().kind == IcEntry::KIND_MEGAMORPHIC {
            return;
        }
        let entry = match recv {
            Value::Object(oid) => self.describe_get(oid, key),
            Value::String(_) => self.describe_string_get(key),
            _ => None,
        };
        if let Some(e) = entry {
            self.ic_insert(site, self.with_key_flags(e, key));
        }
    }

    fn with_key_flags(&self, mut e: IcEntry, key: PropKey) -> IcEntry {
        if key == PropKey::Str(self.names.length)
            && matches!(
                e.kind,
                IcEntry::KIND_OWN_DATA
                    | IcEntry::KIND_OWN_ACCESSOR
                    | IcEntry::KIND_PROTO_DATA
                    | IcEntry::KIND_PROTO_ACCESSOR
            )
        {
            e.flags |= IcEntry::FLAG_LENGTH;
        }
        e
    }

    fn describe_get(&mut self, oid: ObjId, key: PropKey) -> Option<IcEntry> {
        let is_length = key == PropKey::Str(self.names.length);
        let o = self.heap.obj(oid);
        if !receiver_ok(o) {
            return None;
        }
        if is_length {
            match o.class {
                Class::Array(_) => {
                    return Some(IcEntry {
                        kind: IcEntry::KIND_ARRAY_LENGTH,
                        ..IcEntry::EMPTY
                    })
                }
                Class::String(_) => return None,
                _ => {}
            }
        }
        let shape = o.shape;
        if let Some(slot) = self.heap.shape_lookup(shape, key) {
            let kind = match o.slots.get(slot as usize)? {
                PropSlot::Data(_) => IcEntry::KIND_OWN_DATA,
                PropSlot::Accessor { .. } => IcEntry::KIND_OWN_ACCESSOR,
            };
            return Some(IcEntry {
                kind,
                shape: shape.0,
                slot,
                ..IcEntry::EMPTY
            });
        }
        let p1 = o.proto?;
        let (holder, slot, accessor) = self.describe_chain(p1, key, is_length)?;
        Some(IcEntry {
            kind: if accessor {
                IcEntry::KIND_PROTO_ACCESSOR
            } else {
                IcEntry::KIND_PROTO_DATA
            },
            shape: shape.0,
            proto: p1.0,
            holder: holder.0,
            slot,
            epoch: self.heap.proto_epoch,
            ..IcEntry::EMPTY
        })
    }

    fn describe_string_get(&mut self, key: PropKey) -> Option<IcEntry> {
        if key == PropKey::Str(self.names.length) {
            return Some(IcEntry {
                kind: IcEntry::KIND_STRING_LENGTH,
                ..IcEntry::EMPTY
            });
        }
        let sp = self.intrinsic(Intrinsic::StringPrototype);
        let (holder, slot, accessor) = self.describe_chain(sp, key, false)?;
        if accessor {
            return None;
        }
        Some(IcEntry {
            kind: IcEntry::KIND_STRING_PROTO_DATA,
            proto: sp.0,
            holder: holder.0,
            slot,
            epoch: self.heap.proto_epoch,
            ..IcEntry::EMPTY
        })
    }

    /// Find `key` on the chain from `start` (inclusive): the holder, the
    /// slot and whether it is an accessor. Every object from `start` to the
    /// holder becomes watched.
    fn describe_chain(
        &mut self,
        start: ObjId,
        key: PropKey,
        is_length: bool,
    ) -> Option<(ObjId, u32, bool)> {
        if self.heap.proto_epoch == u32::MAX {
            return None;
        }
        let mut cur = start;
        let mut hops = 0;
        let found = loop {
            let h = self.heap.obj(cur);
            if !chain_ok(h, is_length) {
                return None;
            }
            if let Some(slot) = self.heap.shape_lookup(h.shape, key) {
                let accessor = match h.slots.get(slot as usize)? {
                    PropSlot::Data(_) => false,
                    PropSlot::Accessor { .. } => true,
                };
                break (cur, slot, accessor);
            }
            hops += 1;
            if hops > MAX_DEPTH {
                return None;
            }
            cur = h.proto?;
        };
        let mut cur = start;
        loop {
            self.heap.obj_mut(cur).watched = true;
            if cur == found.0 {
                break;
            }
            cur = self.heap.obj(cur).proto.expect("chain walked above");
        }
        Some(found)
    }

    /// Record `e` in `site`: over an entry for the same receivers or a
    /// stale prototype entry, else in a free entry, else the site goes
    /// megamorphic.
    fn ic_insert(&self, site: &IcSite, e: IcEntry) {
        stat!(self, fills);
        let same_receivers = |x: &IcEntry| -> bool {
            let group = |k: u8| match k {
                IcEntry::KIND_ARRAY_LENGTH => 1,
                IcEntry::KIND_STRING_LENGTH | IcEntry::KIND_STRING_PROTO_DATA => 2,
                _ => 0,
            };
            group(x.kind) == group(e.kind)
                && match group(e.kind) {
                    0 => x.shape == e.shape && (x.proto == e.proto || e.proto == u32::MAX || x.proto == u32::MAX),
                    _ => true,
                }
        };
        let stale = |x: &IcEntry| -> bool {
            matches!(
                x.kind,
                IcEntry::KIND_PROTO_DATA
                    | IcEntry::KIND_PROTO_ACCESSOR
                    | IcEntry::KIND_STRING_PROTO_DATA
            ) && x.epoch != self.heap.proto_epoch
        };
        for c in &site.entries {
            let x = c.get();
            if x.kind == IcEntry::KIND_EMPTY || same_receivers(&x) || stale(&x) {
                c.set(e);
                return;
            }
        }
        stat!(self, megamorphic);
        site.entries[0].set(IcEntry {
            kind: IcEntry::KIND_MEGAMORPHIC,
            ..IcEntry::EMPTY
        });
        for c in &site.entries[1..IC_ENTRIES] {
            c.set(IcEntry::EMPTY);
        }
    }

    /// The cached write of an own writable data property; `true` when
    /// done. (Callers count hits and misses.)
    #[inline]
    pub(crate) fn ic_set(&mut self, code: &CodeBlock, ic: IcIdx, oid: ObjId, v: Value) -> bool {
        let Some(site) = self.ic_site(code, ic) else {
            return false;
        };
        let o = self.heap.obj_mut(oid);
        if receiver_ok(o) {
            for c in &site.entries {
                let e = c.get();
                match e.kind {
                    IcEntry::KIND_OWN_DATA if o.shape.0 == e.shape => {
                        if let Some(PropSlot::Data(slot)) = o.slots.get_mut(e.slot as usize) {
                            *slot = v;
                            return true;
                        }
                        break;
                    }
                    IcEntry::KIND_EMPTY | IcEntry::KIND_MEGAMORPHIC => break,
                    _ => {}
                }
            }
        }
        false
    }

    /// The cached setter for a write of `oid` at site `ic`: an own or
    /// inherited accessor with a setter, which the caller calls with `oid`
    /// as `this`.
    pub(crate) fn ic_setter(&self, code: &CodeBlock, ic: IcIdx, oid: ObjId) -> Option<Value> {
        let site = self.ic_site(code, ic)?;
        let o = self.heap.obj(oid);
        if !receiver_ok(o) {
            return None;
        }
        for c in &site.entries {
            let e = c.get();
            if e.flags & IcEntry::FLAG_LENGTH != 0 && length_exotic(o) {
                continue;
            }
            let slot = match e.kind {
                IcEntry::KIND_OWN_ACCESSOR if o.shape.0 == e.shape => o.slots.get(e.slot as usize),
                IcEntry::KIND_PROTO_ACCESSOR
                    if o.shape.0 == e.shape
                        && o.proto.map(|p| p.0) == Some(e.proto)
                        && e.epoch == self.heap.proto_epoch =>
                {
                    self.heap.obj(ObjId(e.holder)).slots.get(e.slot as usize)
                }
                IcEntry::KIND_EMPTY | IcEntry::KIND_MEGAMORPHIC => return None,
                _ => continue,
            };
            return match slot? {
                PropSlot::Accessor { set, .. } if !set.is_undefined() => Some(*set),
                _ => None,
            };
        }
        None
    }

    /// After a miss: record an own writable data property of `oid`, or the
    /// accessor a write of `key` would call.
    pub(crate) fn ic_fill_set(&mut self, code: &CodeBlock, ic: IcIdx, oid: ObjId, key: PropKey) {
        if ic == NO_IC || !matches!(key, PropKey::Str(_)) {
            return;
        }
        let Some(fb) = code.feedback(self.feedback_owner) else {
            return;
        };
        let site = &fb.ic[ic as usize];
        if site.entries[0].get().kind == IcEntry::KIND_MEGAMORPHIC {
            return;
        }
        let is_length = key == PropKey::Str(self.names.length);
        let o = self.heap.obj(oid);
        if !chain_ok(o, is_length) {
            return;
        }
        let shape = o.shape;
        let entry = if let Some(slot) = self.heap.shape_lookup(shape, key) {
            match o.slots.get(slot as usize) {
                Some(PropSlot::Data(_)) if self.heap.shape_key(shape, slot).1.writable() => {
                    IcEntry {
                        kind: IcEntry::KIND_OWN_DATA,
                        shape: shape.0,
                        slot,
                        ..IcEntry::EMPTY
                    }
                }
                Some(PropSlot::Accessor { .. }) => IcEntry {
                    kind: IcEntry::KIND_OWN_ACCESSOR,
                    shape: shape.0,
                    slot,
                    ..IcEntry::EMPTY
                },
                _ => return,
            }
        } else {
            let Some(p1) = o.proto else { return };
            // Only an inherited accessor: an inherited data property means
            // the write adds an own property.
            match self.describe_chain(p1, key, is_length) {
                Some((holder, slot, true)) => IcEntry {
                    kind: IcEntry::KIND_PROTO_ACCESSOR,
                    shape: shape.0,
                    proto: p1.0,
                    holder: holder.0,
                    slot,
                    epoch: self.heap.proto_epoch,
                    ..IcEntry::EMPTY
                },
                _ => return,
            }
        };
        self.ic_insert(site, self.with_key_flags(entry, key));
    }

    // ---- keyed access ----------------------------------------------------------------

    /// `o[n]` for a present dense element.
    #[inline]
    pub(crate) fn keyed_get_fast(&self, oid: ObjId, n: f64) -> Option<Value> {
        let r = (|| {
            let i = index_of(n)?;
            let o = self.heap.obj(oid);
            if !elements_ok(o) {
                return None;
            }
            match &o.elements {
                Elements::Dense(v) => match v.get(i) {
                    Some(x) if !x.is_empty() => Some(*x),
                    _ => None,
                },
                _ => None,
            }
        })();
        if r.is_some() {
            stat!(self, keyed_hits);
        }
        r
    }

    /// `o[n] = v` over a present dense element (always a writable data
    /// property of `o` itself); `true` when done.
    #[inline]
    pub(crate) fn keyed_set_fast(&mut self, oid: ObjId, n: f64, v: Value) -> bool {
        let Some(i) = index_of(n) else {
            return false;
        };
        let o = self.heap.obj_mut(oid);
        if !elements_ok(o) {
            return false;
        }
        if let Elements::Dense(els) = &mut o.elements {
            if let Some(x) = els.get_mut(i) {
                if !x.is_empty() {
                    *x = v;
                    stat!(self, keyed_hits);
                    return true;
                }
            }
        }
        false
    }
}
