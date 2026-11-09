//! Monomorphic inline caches for `GetNamed` / `SetNamed`.
//!
//! An ordinary object's named properties live in `slots`, laid out by its
//! shape. A cache entry remembers, for one access site, the receiver shape
//! it last saw and where the property was found: an own slot, or a slot of
//! the receiver's direct prototype (the common method-call case). A hit
//! needs no key interning, no hashing and no prototype walk.
//!
//! Soundness rests on three facts of the object model: a shape fixes the
//! key *and attributes* of every slot (changing attributes or deleting a
//! property moves the object to dictionary mode); dictionary-mode objects
//! never hit (they are checked explicitly); and shapes are never freed, so
//! a shape id can never come to mean a different layout. Object ids are
//! reused after collection, so a prototype hit re-checks that the
//! receiver's prototype *is* the cached holder and that the holder's shape
//! is unchanged: whatever object now has that id, equal shape means equal
//! layout.
//!
//! Only receivers with no exotic behaviour take part (plain objects,
//! functions, errors, promises, class instances...). Arrays, strings,
//! typed arrays, arguments objects, proxies and module namespaces take the
//! generic path, as do accessors and dictionary-mode objects.

use crate::object::{Class, ObjectData};
use crate::value::{ObjId, PropKey, PropSlot, Value};
use crate::vm::Vm;
use agnijs_js_compiler::bytecode::{CodeBlock, IcEntry, IcIdx, NO_IC};

/// Receivers and holders the cache understands.
#[inline]
fn is_plain(o: &ObjectData) -> bool {
    o.dict.is_none()
        && !matches!(
            o.class,
            Class::Array(_)
                | Class::String(_)
                | Class::Arguments(Some(_))
                | Class::TypedArray(_)
                | Class::Namespace(_)
                | Class::Proxy(_)
        )
}

impl Vm {
    /// The cached read for `oid` at site `ic`, if the entry still applies.
    #[inline]
    pub(crate) fn ic_get(&self, code: &CodeBlock, ic: IcIdx, oid: ObjId) -> Option<Value> {
        if ic == NO_IC {
            return None;
        }
        let e = code.ic[ic as usize].get();
        let o = self.heap.obj(oid);
        if o.shape.0 != e.shape || !is_plain(o) {
            return None;
        }
        let holder = if e.holder == IcEntry::NO_HOLDER {
            o
        } else {
            let p = o.proto?;
            if p.0 != e.holder {
                return None;
            }
            let h = self.heap.obj(p);
            if h.shape.0 != e.holder_shape || !is_plain(h) {
                return None;
            }
            h
        };
        match holder.slots.get(e.slot as usize) {
            Some(PropSlot::Data(v)) => Some(*v),
            _ => None,
        }
    }

    /// After a miss: record where `key` lives for receivers shaped like
    /// `oid`, when the cache can describe it.
    pub(crate) fn ic_fill_get(&mut self, code: &CodeBlock, ic: IcIdx, oid: ObjId, key: PropKey) {
        if ic == NO_IC {
            return;
        }
        let o = self.heap.obj(oid);
        if !is_plain(o) {
            return;
        }
        let shape = o.shape;
        if let Some(slot) = self.heap.shape(shape).lookup(key) {
            if let Some(PropSlot::Data(_)) = o.slots.get(slot as usize) {
                code.ic[ic as usize].set(IcEntry {
                    shape: shape.0,
                    holder: IcEntry::NO_HOLDER,
                    holder_shape: 0,
                    slot,
                });
            }
            return;
        }
        // Not own: a data property on the direct prototype.
        let Some(p) = o.proto else { return };
        let h = self.heap.obj(p);
        if !is_plain(h) {
            return;
        }
        let hshape = h.shape;
        if let Some(slot) = self.heap.shape(hshape).lookup(key) {
            if let Some(PropSlot::Data(_)) = h.slots.get(slot as usize) {
                code.ic[ic as usize].set(IcEntry {
                    shape: shape.0,
                    holder: p.0,
                    holder_shape: hshape.0,
                    slot,
                });
            }
        }
    }

    /// The cached write of an own writable data property; `true` when done.
    #[inline]
    pub(crate) fn ic_set(&mut self, code: &CodeBlock, ic: IcIdx, oid: ObjId, v: Value) -> bool {
        if ic == NO_IC {
            return false;
        }
        let e = code.ic[ic as usize].get();
        let o = self.heap.obj_mut(oid);
        if o.shape.0 != e.shape || e.holder != IcEntry::NO_HOLDER || !is_plain(o) {
            return false;
        }
        match o.slots.get_mut(e.slot as usize) {
            Some(PropSlot::Data(slot)) => {
                *slot = v;
                true
            }
            _ => false,
        }
    }

    /// After a miss: record an own writable data property of `oid`.
    pub(crate) fn ic_fill_set(&mut self, code: &CodeBlock, ic: IcIdx, oid: ObjId, key: PropKey) {
        if ic == NO_IC {
            return;
        }
        let o = self.heap.obj(oid);
        if !is_plain(o) {
            return;
        }
        let shape = o.shape;
        let sh = self.heap.shape(shape);
        if let Some(slot) = sh.lookup(key) {
            let writable = sh.keys[slot as usize].1.writable();
            if writable && matches!(o.slots.get(slot as usize), Some(PropSlot::Data(_))) {
                code.ic[ic as usize].set(IcEntry {
                    shape: shape.0,
                    holder: IcEntry::NO_HOLDER,
                    holder_shape: 0,
                    slot,
                });
            }
        }
    }
}
