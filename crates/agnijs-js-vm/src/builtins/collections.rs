//! Map, Set, WeakMap, WeakSet, WeakRef and FinalizationRegistry (ES2025
//! §24.1–24.4 and §26.1–26.2), including the ES2025 Set methods, plus the
//! weak-reference processing the collector calls between mark and sweep.

use agnijs_engine_api::RealmId;

use crate::gc::Tracer;
use crate::object::{CallArgs, Class, IterKind, ObjectData};
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, PropKey, Value};
use crate::vm::{JsResult, Job, Vm};

/// Insertion-ordered entries with tombstones so live iterators survive
/// deletions; `index` maps a normalised key to its entry slot.
pub struct MapData {
    pub entries: Vec<Option<(Value, Value)>>,
    pub index: hashbrown::HashMap<MapKey, usize>,
    pub live: usize,
    pub weak: bool,
}

/// A SameValueZero-normalised key: `-0` is `+0`, NaN equals NaN, strings
/// compare by content.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum MapKey {
    Undefined,
    Null,
    Bool(bool),
    Number(u64),
    String(Vec<u16>),
    Symbol(crate::value::SymId),
    BigInt(Vec<u8>),
    Object(ObjId),
}

impl MapData {
    pub fn new(weak: bool) -> MapData {
        MapData {
            entries: Vec::new(),
            index: hashbrown::HashMap::new(),
            live: 0,
            weak,
        }
    }

    /// Strong collections trace keys and values. Weak ones trace nothing
    /// here: their values are reached through the collector's ephemeron
    /// pass only while the key is alive (`gc.rs`).
    pub fn trace(&self, t: &mut Tracer) {
        if self.weak {
            return;
        }
        for e in self.entries.iter().flatten() {
            t.value(e.0);
            t.value(e.1);
        }
    }

    pub fn get(&self, key: &MapKey) -> Option<Value> {
        self.index
            .get(key)
            .and_then(|&i| self.entries[i].as_ref().map(|e| e.1))
    }

    pub fn has(&self, key: &MapKey) -> bool {
        self.index.contains_key(key)
    }

    /// Insert or overwrite; returns `true` when the key was new.
    pub fn set(&mut self, key: MapKey, k: Value, v: Value) -> bool {
        if let Some(&i) = self.index.get(&key) {
            if let Some(e) = &mut self.entries[i] {
                e.1 = v;
            }
            return false;
        }
        self.index.insert(key, self.entries.len());
        self.entries.push(Some((k, v)));
        self.live += 1;
        true
    }

    pub fn remove(&mut self, key: &MapKey) -> bool {
        match self.index.remove(key) {
            Some(i) => {
                self.entries[i] = None;
                self.live -= 1;
                true
            }
            None => false,
        }
    }

    pub fn clear(&mut self) {
        for e in self.entries.iter_mut() {
            *e = None;
        }
        self.index.clear();
        self.live = 0;
    }

    /// A compacted copy in insertion order (no live iterators point into
    /// a fresh copy, so tombstones can go).
    fn copy(&self) -> MapData {
        let mut by_slot: Vec<(usize, &MapKey)> = self.index.iter().map(|(k, &i)| (i, k)).collect();
        by_slot.sort_by_key(|(i, _)| *i);
        let mut out = MapData::new(self.weak);
        for (i, key) in by_slot {
            if let Some((k, v)) = self.entries[i] {
                out.set(key.clone(), k, v);
            }
        }
        out
    }
}

pub struct MapIterState {
    pub target: Option<ObjId>,
    pub index: usize,
    pub kind: IterKind,
}

pub struct FinRegData {
    pub cleanup: Value,
    pub cells: Vec<FinCell>,
}

pub struct FinCell {
    pub target: Value,
    pub held: Value,
    /// `Undefined` when registered without a token.
    pub token: Value,
}

impl FinRegData {
    pub fn trace(&self, t: &mut Tracer) {
        t.value(self.cleanup);
        for c in &self.cells {
            // Targets and tokens are weak.
            t.value(c.held);
        }
    }
}

// ---- weak processing (called by the collector) -------------------------------------------

/// Clear weak references whose targets died in this collection: weak
/// collection entries, WeakRef targets, and FinalizationRegistry cells
/// (whose cleanup callbacks are queued as host-drained jobs). `weak` lists
/// the marked weak containers; the collector records them while marking
/// (ephemeron values were marked there too), so no arena scan is needed.
pub fn process_weak(vm: &mut Vm, weak: &[ObjId]) {
    let mut weak_colls: Vec<ObjId> = Vec::new();
    let mut weak_refs: Vec<ObjId> = Vec::new();
    let mut registries: Vec<ObjId> = Vec::new();
    for &o in weak {
        match &vm.heap.obj(o).class {
            Class::WeakMap(_) | Class::WeakSet(_) => weak_colls.push(o),
            Class::WeakRef(_) => weak_refs.push(o),
            Class::FinalizationRegistry(_) => registries.push(o),
            _ => {}
        }
    }
    for o in weak_colls {
        let dead: Vec<MapKey> = match &vm.heap.obj(o).class {
            Class::WeakMap(m) | Class::WeakSet(m) => m
                .index
                .iter()
                .filter(|(_, &i)| {
                    m.entries[i]
                        .map(|(k, _)| !vm.is_value_marked(k))
                        .unwrap_or(false)
                })
                .map(|(k, _)| k.clone())
                .collect(),
            _ => Vec::new(),
        };
        if dead.is_empty() {
            continue;
        }
        if let Class::WeakMap(m) | Class::WeakSet(m) = &mut vm.heap.obj_mut(o).class {
            for k in &dead {
                m.remove(k);
            }
        }
    }
    for o in weak_refs {
        let target = match &vm.heap.obj(o).class {
            Class::WeakRef(v) => *v,
            _ => continue,
        };
        if !target.is_undefined() && !vm.is_value_marked(target) {
            vm.heap.obj_mut(o).class = Class::WeakRef(Value::Undefined);
        }
    }
    for o in registries {
        let (cleanup, dead_cells, cleared_tokens) = match &vm.heap.obj(o).class {
            Class::FinalizationRegistry(f) => {
                let mut dead = Vec::new();
                let mut tokens = Vec::new();
                for (i, c) in f.cells.iter().enumerate() {
                    if !vm.is_value_marked(c.target) {
                        dead.push(i);
                    } else if !c.token.is_undefined() && !vm.is_value_marked(c.token) {
                        tokens.push(i);
                    }
                }
                (f.cleanup, dead, tokens)
            }
            _ => continue,
        };
        if dead_cells.is_empty() && cleared_tokens.is_empty() {
            continue;
        }
        let mut held: Vec<Value> = Vec::new();
        if let Class::FinalizationRegistry(f) = &mut vm.heap.obj_mut(o).class {
            for &i in &cleared_tokens {
                f.cells[i].token = Value::Undefined;
            }
            for &i in dead_cells.iter().rev() {
                let c = f.cells.remove(i);
                held.push(c.held);
            }
        }
        // Cells were removed back to front; callbacks run in registration order.
        for h in held.into_iter().rev() {
            vm.jobs.push_back(Job::Call {
                func: cleanup,
                args: vec![h],
            });
        }
    }
}

// ---- keys --------------------------------------------------------------------------------

/// The SameValueZero identity of `v`.
fn map_key(vm: &mut Vm, v: Value) -> MapKey {
    match v {
        Value::Undefined | Value::Empty => MapKey::Undefined,
        Value::Null => MapKey::Null,
        Value::Bool(b) => MapKey::Bool(b),
        Value::Number(n) => {
            let n = if n == 0.0 { 0.0 } else if n.is_nan() { f64::NAN } else { n };
            MapKey::Number(n.to_bits())
        }
        Value::String(s) => MapKey::String(vm.heap.units(s).to_vec()),
        Value::Symbol(s) => MapKey::Symbol(s),
        Value::BigInt(b) => MapKey::BigInt(vm.heap.bigint(b).0.to_signed_bytes_le()),
        Value::Object(o) => MapKey::Object(o),
    }
}

/// `-0` is stored as `+0`.
fn normalize(v: Value) -> Value {
    match v {
        Value::Number(n) if n == 0.0 => Value::Number(0.0),
        _ => v,
    }
}

/// SameValue for the objects and symbols that can be held weakly.
fn same_identity(a: Value, b: Value) -> bool {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => x == y,
        (Value::Symbol(x), Value::Symbol(y)) => x == y,
        _ => false,
    }
}

/// CanBeHeldWeakly(v): objects and unregistered symbols.
fn can_be_held_weakly(vm: &Vm, v: Value) -> bool {
    match v {
        Value::Object(_) => true,
        Value::Symbol(s) => !vm.symbol_registry.values().any(|&r| r == s),
        _ => false,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Map,
    Set,
    WeakMap,
    WeakSet,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Map => "Map",
            Kind::Set => "Set",
            Kind::WeakMap => "WeakMap",
            Kind::WeakSet => "WeakSet",
        }
    }
    fn matches(self, c: &Class) -> bool {
        matches!(
            (self, c),
            (Kind::Map, Class::Map(_))
                | (Kind::Set, Class::Set(_))
                | (Kind::WeakMap, Class::WeakMap(_))
                | (Kind::WeakSet, Class::WeakSet(_))
        )
    }
}

fn this_coll(vm: &mut Vm, this: Value, kind: Kind, method: &str) -> JsResult<ObjId> {
    if let Value::Object(o) = this {
        if kind.matches(&vm.heap.obj(o).class) {
            return Ok(o);
        }
    }
    vm.type_error(&format!(
        "{}.prototype.{method} called on incompatible receiver",
        kind.name()
    ))
}

fn data(vm: &mut Vm, o: ObjId) -> &mut MapData {
    match &mut vm.heap.obj_mut(o).class {
        Class::Map(m) | Class::Set(m) | Class::WeakMap(m) | Class::WeakSet(m) => m,
        _ => unreachable!("collection data on a non-collection"),
    }
}

fn data_ref(vm: &Vm, o: ObjId) -> &MapData {
    match &vm.heap.obj(o).class {
        Class::Map(m) | Class::Set(m) | Class::WeakMap(m) | Class::WeakSet(m) => m,
        _ => unreachable!("collection data on a non-collection"),
    }
}

// ---- install -----------------------------------------------------------------------------

pub fn install(vm: &mut Vm, realm: RealmId) {
    let iter_proto = vm.intrinsic(Intrinsic::IteratorPrototype);

    // Map
    let proto = vm.new_object();
    let ctor = super::install_ctor(vm, realm, "Map", 0, map_constructor, proto, Intrinsic::Map, Intrinsic::MapPrototype);
    super::species_accessor(vm, ctor);
    vm.method(ctor, "groupBy", 2, map_group_by);
    vm.method(proto, "clear", 0, map_clear);
    vm.method(proto, "delete", 1, map_delete);
    let entries = vm.method(proto, "entries", 0, map_entries);
    vm.method(proto, "forEach", 1, map_for_each);
    vm.method(proto, "get", 1, map_get);
    vm.method(proto, "has", 1, map_has);
    vm.method(proto, "keys", 0, map_keys);
    vm.method(proto, "set", 2, map_set);
    vm.accessor(proto, "size", map_size, None);
    vm.method(proto, "values", 0, map_values);
    let it = PropKey::Sym(vm.well_known.iterator);
    vm.define_raw(proto, it, Value::Object(entries), Attrs::METHOD);
    super::to_string_tag(vm, proto, "Map");

    let mip = vm.new_object_with_proto(Some(iter_proto));
    vm.realm_mut().set_intrinsic(Intrinsic::MapIteratorPrototype, mip);
    vm.method(mip, "next", 0, map_iterator_next);
    super::to_string_tag(vm, mip, "Map Iterator");

    // Set
    let proto = vm.new_object();
    let ctor = super::install_ctor(vm, realm, "Set", 0, set_constructor, proto, Intrinsic::Set, Intrinsic::SetPrototype);
    super::species_accessor(vm, ctor);
    vm.method(proto, "add", 1, set_add);
    vm.method(proto, "clear", 0, set_clear);
    vm.method(proto, "delete", 1, set_delete);
    vm.method(proto, "difference", 1, set_difference);
    vm.method(proto, "entries", 0, set_entries);
    vm.method(proto, "forEach", 1, set_for_each);
    vm.method(proto, "has", 1, set_has);
    vm.method(proto, "intersection", 1, set_intersection);
    vm.method(proto, "isDisjointFrom", 1, set_is_disjoint_from);
    vm.method(proto, "isSubsetOf", 1, set_is_subset_of);
    vm.method(proto, "isSupersetOf", 1, set_is_superset_of);
    vm.accessor(proto, "size", set_size, None);
    vm.method(proto, "symmetricDifference", 1, set_symmetric_difference);
    vm.method(proto, "union", 1, set_union);
    let values = vm.method(proto, "values", 0, set_values);
    vm.define_raw_str(proto, "keys", Value::Object(values), Attrs::METHOD);
    let it = PropKey::Sym(vm.well_known.iterator);
    vm.define_raw(proto, it, Value::Object(values), Attrs::METHOD);
    super::to_string_tag(vm, proto, "Set");

    let sip = vm.new_object_with_proto(Some(iter_proto));
    vm.realm_mut().set_intrinsic(Intrinsic::SetIteratorPrototype, sip);
    vm.method(sip, "next", 0, set_iterator_next);
    super::to_string_tag(vm, sip, "Set Iterator");

    // WeakMap
    let proto = vm.new_object();
    super::install_ctor(vm, realm, "WeakMap", 0, weakmap_constructor, proto, Intrinsic::WeakMap, Intrinsic::WeakMapPrototype);
    vm.method(proto, "delete", 1, weakmap_delete);
    vm.method(proto, "get", 1, weakmap_get);
    vm.method(proto, "has", 1, weakmap_has);
    vm.method(proto, "set", 2, weakmap_set);
    super::to_string_tag(vm, proto, "WeakMap");

    // WeakSet
    let proto = vm.new_object();
    super::install_ctor(vm, realm, "WeakSet", 0, weakset_constructor, proto, Intrinsic::WeakSet, Intrinsic::WeakSetPrototype);
    vm.method(proto, "add", 1, weakset_add);
    vm.method(proto, "delete", 1, weakset_delete);
    vm.method(proto, "has", 1, weakset_has);
    super::to_string_tag(vm, proto, "WeakSet");

    // WeakRef
    let proto = vm.new_object();
    super::install_ctor(vm, realm, "WeakRef", 1, weakref_constructor, proto, Intrinsic::WeakRef, Intrinsic::WeakRefPrototype);
    vm.method(proto, "deref", 0, weakref_deref);
    super::to_string_tag(vm, proto, "WeakRef");

    // FinalizationRegistry
    let proto = vm.new_object();
    super::install_ctor(
        vm,
        realm,
        "FinalizationRegistry",
        1,
        finreg_constructor,
        proto,
        Intrinsic::FinalizationRegistry,
        Intrinsic::FinalizationRegistryPrototype,
    );
    vm.method(proto, "register", 2, finreg_register);
    vm.method(proto, "unregister", 1, finreg_unregister);
    super::to_string_tag(vm, proto, "FinalizationRegistry");
}

// ---- constructors ------------------------------------------------------------------------

/// The shared constructor algorithm: allocate from `new.target`, then feed
/// the iterable through the `adder` method (`set` or `add`).
fn construct_collection(vm: &mut Vm, a: &CallArgs, kind: Kind, proto_intrinsic: Intrinsic) -> JsResult<Value> {
    if a.new_target.is_undefined() {
        return vm.type_error(&format!("Constructor {} requires 'new'", kind.name()));
    }
    let proto = vm.get_prototype_from_constructor(a.new_target, proto_intrinsic)?;
    let md = Box::new(MapData::new(matches!(kind, Kind::WeakMap | Kind::WeakSet)));
    let class = match kind {
        Kind::Map => Class::Map(md),
        Kind::Set => Class::Set(md),
        Kind::WeakMap => Class::WeakMap(md),
        Kind::WeakSet => Class::WeakSet(md),
    };
    let o = vm.alloc(ObjectData::new(Some(proto), class));
    let iterable = a.arg(0);
    if iterable.is_nullish() {
        return Ok(Value::Object(o));
    }
    let pairs = matches!(kind, Kind::Map | Kind::WeakMap);
    let adder_name = if pairs { vm.names.set } else { vm.names.add };
    let adder = vm.get(o, PropKey::Str(adder_name))?;
    if !vm.is_callable(adder) {
        return vm.type_error(&format!(
            "{}.prototype.{} is not a function",
            kind.name(),
            if pairs { "set" } else { "add" }
        ));
    }
    let (iter, next) = vm.get_iterator(iterable, false)?;
    loop {
        let Some(item) = vm.iterator_step_value(iter, next)? else {
            return Ok(Value::Object(o));
        };
        let r: JsResult<()> = if pairs {
            add_entry_from_pair(vm, o, adder, item)
        } else {
            vm.call_value(adder, Value::Object(o), &[item]).map(|_| ())
        };
        if let Err(f) = r {
            vm.iterator_close_swallow(iter);
            return Err(f);
        }
    }
}

fn add_entry_from_pair(vm: &mut Vm, o: ObjId, adder: Value, item: Value) -> JsResult<()> {
    let Value::Object(io) = item else {
        return vm.type_error("Iterator value is not an entry object");
    };
    let k = vm.get(io, PropKey::Index(0))?;
    let v = vm.get(io, PropKey::Index(1))?;
    vm.call_value(adder, Value::Object(o), &[k, v])?;
    Ok(())
}

fn map_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    construct_collection(vm, a, Kind::Map, Intrinsic::MapPrototype)
}
fn set_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    construct_collection(vm, a, Kind::Set, Intrinsic::SetPrototype)
}
fn weakmap_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    construct_collection(vm, a, Kind::WeakMap, Intrinsic::WeakMapPrototype)
}
fn weakset_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    construct_collection(vm, a, Kind::WeakSet, Intrinsic::WeakSetPrototype)
}

/// Map.groupBy(items, callback).
fn map_group_by(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let groups = super::object::group_by(vm, a.arg(0), a.arg(1), false)?;
    let proto = vm.intrinsic(Intrinsic::MapPrototype);
    let o = vm.alloc(ObjectData::new(Some(proto), Class::Map(Box::new(MapData::new(false)))));
    for (key, items) in groups {
        let key = normalize(key);
        let mk = map_key(vm, key);
        let arr = Value::Object(vm.new_array(items));
        data(vm, o).set(mk, key, arr);
    }
    Ok(Value::Object(o))
}

// ---- Map.prototype -----------------------------------------------------------------------

fn map_clear(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::Map, "clear")?;
    data(vm, o).clear();
    Ok(Value::Undefined)
}

fn map_delete(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::Map, "delete")?;
    let k = map_key(vm, a.arg(0));
    Ok(Value::Bool(data(vm, o).remove(&k)))
}

fn map_get(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::Map, "get")?;
    let k = map_key(vm, a.arg(0));
    Ok(data(vm, o).get(&k).unwrap_or(Value::Undefined))
}

fn map_has(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::Map, "has")?;
    let k = map_key(vm, a.arg(0));
    Ok(Value::Bool(data(vm, o).has(&k)))
}

fn map_set(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::Map, "set")?;
    let key = normalize(a.arg(0));
    let k = map_key(vm, key);
    data(vm, o).set(k, key, a.arg(1));
    Ok(a.this)
}

fn map_size(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::Map, "size")?;
    Ok(Value::Number(data(vm, o).live as f64))
}

fn map_for_each(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::Map, "forEach")?;
    let f = a.arg(0);
    if !vm.is_callable(f) {
        return vm.type_error("Map.prototype.forEach callback is not a function");
    }
    let this_arg = a.arg(1);
    // Series (`Vm::callback_series`): the entry is read from the map and
    // passed straight to the call; nothing else is held between calls.
    vm.callback_series(&[a.this, f, this_arg], |vm| {
        let mut i = 0;
        loop {
            let entry = {
                let d = data_ref(vm, o);
                if i >= d.entries.len() {
                    break;
                }
                d.entries[i]
            };
            i += 1;
            if let Some((k, v)) = entry {
                vm.call_value(f, this_arg, &[v, k, a.this])?;
            }
        }
        Ok(Value::Undefined)
    })
}

fn create_map_iterator(vm: &mut Vm, o: ObjId, kind: IterKind, set: bool) -> Value {
    let proto = vm.intrinsic(if set {
        Intrinsic::SetIteratorPrototype
    } else {
        Intrinsic::MapIteratorPrototype
    });
    let st = Box::new(MapIterState {
        target: Some(o),
        index: 0,
        kind,
    });
    let class = if set {
        Class::SetIterator(st)
    } else {
        Class::MapIterator(st)
    };
    Value::Object(vm.alloc(ObjectData::new(Some(proto), class)))
}

fn map_entries(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::Map, "entries")?;
    Ok(create_map_iterator(vm, o, IterKind::Entries, false))
}
fn map_keys(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::Map, "keys")?;
    Ok(create_map_iterator(vm, o, IterKind::Keys, false))
}
fn map_values(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::Map, "values")?;
    Ok(create_map_iterator(vm, o, IterKind::Values, false))
}

/// `%MapIteratorPrototype%.next` and `%SetIteratorPrototype%.next`.
fn iterator_next(vm: &mut Vm, a: &CallArgs, set: bool) -> JsResult<Value> {
    let name = if set { "Set" } else { "Map" };
    let Some(o) = a.this.as_object() else {
        return vm.type_error(&format!("%{name}IteratorPrototype%.next called on incompatible receiver"));
    };
    let (target, index, kind) = match (&vm.heap.obj(o).class, set) {
        (Class::MapIterator(s), false) | (Class::SetIterator(s), true) => (s.target, s.index, s.kind),
        _ => {
            return vm.type_error(&format!("%{name}IteratorPrototype%.next called on incompatible receiver"))
        }
    };
    let Some(t) = target else {
        return Ok(vm.create_iter_result(Value::Undefined, true));
    };
    let mut i = index;
    let found = loop {
        let d = data_ref(vm, t);
        if i >= d.entries.len() {
            break None;
        }
        let e = d.entries[i];
        i += 1;
        if let Some(e) = e {
            break Some(e);
        }
    };
    match found {
        None => {
            if let Class::MapIterator(s) | Class::SetIterator(s) = &mut vm.heap.obj_mut(o).class {
                s.target = None;
            }
            Ok(vm.create_iter_result(Value::Undefined, true))
        }
        Some((k, v)) => {
            if let Class::MapIterator(s) | Class::SetIterator(s) = &mut vm.heap.obj_mut(o).class {
                s.index = i;
            }
            let value = match kind {
                IterKind::Keys => k,
                IterKind::Values => {
                    if set {
                        k
                    } else {
                        v
                    }
                }
                IterKind::Entries => {
                    let second = if set { k } else { v };
                    Value::Object(vm.new_array(vec![k, second]))
                }
            };
            Ok(vm.create_iter_result(value, false))
        }
    }
}

fn map_iterator_next(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    iterator_next(vm, a, false)
}
fn set_iterator_next(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    iterator_next(vm, a, true)
}

// ---- Set.prototype -----------------------------------------------------------------------

fn set_add(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::Set, "add")?;
    let v = normalize(a.arg(0));
    let k = map_key(vm, v);
    data(vm, o).set(k, v, Value::Undefined);
    Ok(a.this)
}

fn set_clear(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::Set, "clear")?;
    data(vm, o).clear();
    Ok(Value::Undefined)
}

fn set_delete(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::Set, "delete")?;
    let k = map_key(vm, a.arg(0));
    Ok(Value::Bool(data(vm, o).remove(&k)))
}

fn set_has(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::Set, "has")?;
    let k = map_key(vm, a.arg(0));
    Ok(Value::Bool(data(vm, o).has(&k)))
}

fn set_size(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::Set, "size")?;
    Ok(Value::Number(data(vm, o).live as f64))
}

fn set_for_each(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::Set, "forEach")?;
    let f = a.arg(0);
    if !vm.is_callable(f) {
        return vm.type_error("Set.prototype.forEach callback is not a function");
    }
    let this_arg = a.arg(1);
    // Series, as for Map.prototype.forEach.
    vm.callback_series(&[a.this, f, this_arg], |vm| {
        let mut i = 0;
        loop {
            let entry = {
                let d = data_ref(vm, o);
                if i >= d.entries.len() {
                    break;
                }
                d.entries[i]
            };
            i += 1;
            if let Some((k, _)) = entry {
                vm.call_value(f, this_arg, &[k, k, a.this])?;
            }
        }
        Ok(Value::Undefined)
    })
}

fn set_entries(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::Set, "entries")?;
    Ok(create_map_iterator(vm, o, IterKind::Entries, true))
}
fn set_values(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::Set, "values")?;
    Ok(create_map_iterator(vm, o, IterKind::Values, true))
}

// ---- ES2025 Set methods --------------------------------------------------------------------

struct SetRecord {
    set: Value,
    size: f64,
    has: Value,
    keys: Value,
}

/// GetSetRecord(obj).
fn get_set_record(vm: &mut Vm, obj: Value) -> JsResult<SetRecord> {
    let Value::Object(o) = obj else {
        return vm.type_error("Set operation argument is not an object");
    };
    let raw_size = vm.get(o, PropKey::Str(vm.names.size))?;
    let num_size = vm.to_number(raw_size)?;
    if num_size.is_nan() {
        return vm.type_error("Set operation argument has a NaN size");
    }
    let int_size = vm.to_integer_or_infinity(Value::Number(num_size))?;
    if int_size < 0.0 {
        return vm.range_error("Set operation argument has a negative size");
    }
    let has = vm.get(o, PropKey::Str(vm.names.has))?;
    if !vm.is_callable(has) {
        return vm.type_error("Set operation argument has no callable 'has'");
    }
    let keys = vm.get(o, PropKey::Str(vm.names.keys))?;
    if !vm.is_callable(keys) {
        return vm.type_error("Set operation argument has no callable 'keys'");
    }
    Ok(SetRecord {
        set: obj,
        size: int_size,
        has,
        keys,
    })
}

/// GetIteratorFromMethod(setRec.[[Set]], setRec.[[Keys]]).
fn keys_iterator(vm: &mut Vm, rec: &SetRecord) -> JsResult<(Value, Value)> {
    let iter = vm.call_value(rec.keys, rec.set, &[])?;
    if !iter.is_object() {
        return vm.type_error("Result of the keys method is not an object");
    }
    let next = vm.get_value(iter, PropKey::Str(vm.names.next))?;
    Ok((iter, next))
}

fn call_has(vm: &mut Vm, rec: &SetRecord, v: Value) -> JsResult<bool> {
    let r = vm.call_value(rec.has, rec.set, &[v])?;
    Ok(vm.to_boolean(r))
}

/// The raw entry at `i` of the receiver's SetData (None when `i` is past
/// the end, Some(None) for an empty slot).
fn raw_entry(vm: &Vm, o: ObjId, i: usize) -> Option<Option<Value>> {
    let d = data_ref(vm, o);
    d.entries.get(i).map(|e| e.map(|(k, _)| k))
}

fn new_set_from(vm: &mut Vm, md: MapData) -> Value {
    let proto = vm.intrinsic(Intrinsic::SetPrototype);
    Value::Object(vm.alloc(ObjectData::new(Some(proto), Class::Set(Box::new(md)))))
}

fn set_union(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::Set, "union")?;
    let rec = get_set_record(vm, a.arg(0))?;
    let (iter, next) = keys_iterator(vm, &rec)?;
    let mut result = data_ref(vm, o).copy();
    while let Some(v) = vm.iterator_step_value(iter, next)? {
        let v = normalize(v);
        let k = map_key(vm, v);
        result.set(k, v, Value::Undefined);
    }
    Ok(new_set_from(vm, result))
}

fn set_intersection(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::Set, "intersection")?;
    let rec = get_set_record(vm, a.arg(0))?;
    let mut result = MapData::new(false);
    let this_size = data_ref(vm, o).live as f64;
    if this_size <= rec.size {
        let mut i = 0;
        while let Some(slot) = raw_entry(vm, o, i) {
            i += 1;
            if let Some(e) = slot {
                if call_has(vm, &rec, e)? {
                    let k = map_key(vm, e);
                    result.set(k, e, Value::Undefined);
                }
            }
        }
    } else {
        let (iter, next) = keys_iterator(vm, &rec)?;
        while let Some(v) = vm.iterator_step_value(iter, next)? {
            let v = normalize(v);
            let k = map_key(vm, v);
            if data_ref(vm, o).has(&k) {
                result.set(k, v, Value::Undefined);
            }
        }
    }
    Ok(new_set_from(vm, result))
}

fn set_difference(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::Set, "difference")?;
    let rec = get_set_record(vm, a.arg(0))?;
    let mut result = data_ref(vm, o).copy();
    let this_size = data_ref(vm, o).live as f64;
    if this_size <= rec.size {
        let mut i = 0;
        while let Some(slot) = raw_entry(vm, o, i) {
            i += 1;
            if let Some(e) = slot {
                if call_has(vm, &rec, e)? {
                    let k = map_key(vm, e);
                    result.remove(&k);
                }
            }
        }
    } else {
        let (iter, next) = keys_iterator(vm, &rec)?;
        while let Some(v) = vm.iterator_step_value(iter, next)? {
            let k = map_key(vm, normalize(v));
            result.remove(&k);
        }
    }
    Ok(new_set_from(vm, result))
}

fn set_symmetric_difference(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::Set, "symmetricDifference")?;
    let rec = get_set_record(vm, a.arg(0))?;
    let (iter, next) = keys_iterator(vm, &rec)?;
    let mut result = data_ref(vm, o).copy();
    while let Some(v) = vm.iterator_step_value(iter, next)? {
        let v = normalize(v);
        let k = map_key(vm, v);
        if data_ref(vm, o).has(&k) {
            result.remove(&k);
        } else {
            result.set(k, v, Value::Undefined);
        }
    }
    Ok(new_set_from(vm, result))
}

fn set_is_subset_of(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::Set, "isSubsetOf")?;
    let rec = get_set_record(vm, a.arg(0))?;
    if (data_ref(vm, o).live as f64) > rec.size {
        return Ok(Value::Bool(false));
    }
    let mut i = 0;
    while let Some(slot) = raw_entry(vm, o, i) {
        i += 1;
        if let Some(e) = slot {
            if !call_has(vm, &rec, e)? {
                return Ok(Value::Bool(false));
            }
        }
    }
    Ok(Value::Bool(true))
}

fn set_is_superset_of(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::Set, "isSupersetOf")?;
    let rec = get_set_record(vm, a.arg(0))?;
    if (data_ref(vm, o).live as f64) < rec.size {
        return Ok(Value::Bool(false));
    }
    let (iter, next) = keys_iterator(vm, &rec)?;
    while let Some(v) = vm.iterator_step_value(iter, next)? {
        let k = map_key(vm, v);
        if !data_ref(vm, o).has(&k) {
            vm.iterator_close(iter)?;
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Bool(true))
}

fn set_is_disjoint_from(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::Set, "isDisjointFrom")?;
    let rec = get_set_record(vm, a.arg(0))?;
    if (data_ref(vm, o).live as f64) <= rec.size {
        let mut i = 0;
        while let Some(slot) = raw_entry(vm, o, i) {
            i += 1;
            if let Some(e) = slot {
                if call_has(vm, &rec, e)? {
                    return Ok(Value::Bool(false));
                }
            }
        }
    } else {
        let (iter, next) = keys_iterator(vm, &rec)?;
        while let Some(v) = vm.iterator_step_value(iter, next)? {
            let k = map_key(vm, v);
            if data_ref(vm, o).has(&k) {
                vm.iterator_close(iter)?;
                return Ok(Value::Bool(false));
            }
        }
    }
    Ok(Value::Bool(true))
}

// ---- WeakMap / WeakSet -------------------------------------------------------------------

fn weakmap_delete(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::WeakMap, "delete")?;
    if !can_be_held_weakly(vm, a.arg(0)) {
        return Ok(Value::Bool(false));
    }
    let k = map_key(vm, a.arg(0));
    Ok(Value::Bool(data(vm, o).remove(&k)))
}

fn weakmap_get(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::WeakMap, "get")?;
    if !can_be_held_weakly(vm, a.arg(0)) {
        return Ok(Value::Undefined);
    }
    let k = map_key(vm, a.arg(0));
    Ok(data(vm, o).get(&k).unwrap_or(Value::Undefined))
}

fn weakmap_has(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::WeakMap, "has")?;
    if !can_be_held_weakly(vm, a.arg(0)) {
        return Ok(Value::Bool(false));
    }
    let k = map_key(vm, a.arg(0));
    Ok(Value::Bool(data(vm, o).has(&k)))
}

fn weakmap_set(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::WeakMap, "set")?;
    let key = a.arg(0);
    if !can_be_held_weakly(vm, key) {
        return vm.type_error("Invalid value used as weak map key");
    }
    let k = map_key(vm, key);
    data(vm, o).set(k, key, a.arg(1));
    Ok(a.this)
}

fn weakset_add(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::WeakSet, "add")?;
    let v = a.arg(0);
    if !can_be_held_weakly(vm, v) {
        return vm.type_error("Invalid value used in weak set");
    }
    let k = map_key(vm, v);
    data(vm, o).set(k, v, Value::Undefined);
    Ok(a.this)
}

fn weakset_delete(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::WeakSet, "delete")?;
    if !can_be_held_weakly(vm, a.arg(0)) {
        return Ok(Value::Bool(false));
    }
    let k = map_key(vm, a.arg(0));
    Ok(Value::Bool(data(vm, o).remove(&k)))
}

fn weakset_has(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_coll(vm, a.this, Kind::WeakSet, "has")?;
    if !can_be_held_weakly(vm, a.arg(0)) {
        return Ok(Value::Bool(false));
    }
    let k = map_key(vm, a.arg(0));
    Ok(Value::Bool(data(vm, o).has(&k)))
}

// ---- WeakRef -----------------------------------------------------------------------------

fn weakref_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if a.new_target.is_undefined() {
        return vm.type_error("Constructor WeakRef requires 'new'");
    }
    let target = a.arg(0);
    if !can_be_held_weakly(vm, target) {
        return vm.type_error("WeakRef: target must be an object or a non-registered symbol");
    }
    let proto = vm.get_prototype_from_constructor(a.new_target, Intrinsic::WeakRefPrototype)?;
    vm.keep_alive.push(target);
    Ok(Value::Object(vm.alloc(ObjectData::new(Some(proto), Class::WeakRef(target)))))
}

fn weakref_deref(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let target = match a.this {
        Value::Object(o) => match &vm.heap.obj(o).class {
            Class::WeakRef(v) => *v,
            _ => return vm.type_error("WeakRef.prototype.deref called on incompatible receiver"),
        },
        _ => return vm.type_error("WeakRef.prototype.deref called on incompatible receiver"),
    };
    if !target.is_undefined() {
        vm.keep_alive.push(target);
    }
    Ok(target)
}

// ---- FinalizationRegistry ----------------------------------------------------------------

fn finreg_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if a.new_target.is_undefined() {
        return vm.type_error("Constructor FinalizationRegistry requires 'new'");
    }
    let cleanup = a.arg(0);
    if !vm.is_callable(cleanup) {
        return vm.type_error("FinalizationRegistry: cleanup must be callable");
    }
    let proto = vm.get_prototype_from_constructor(a.new_target, Intrinsic::FinalizationRegistryPrototype)?;
    let d = FinRegData {
        cleanup,
        cells: Vec::new(),
    };
    Ok(Value::Object(vm.alloc(ObjectData::new(
        Some(proto),
        Class::FinalizationRegistry(Box::new(d)),
    ))))
}

fn this_finreg(vm: &mut Vm, this: Value, method: &str) -> JsResult<ObjId> {
    if let Value::Object(o) = this {
        if matches!(vm.heap.obj(o).class, Class::FinalizationRegistry(_)) {
            return Ok(o);
        }
    }
    vm.type_error(&format!(
        "FinalizationRegistry.prototype.{method} called on incompatible receiver"
    ))
}

fn finreg_register(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_finreg(vm, a.this, "register")?;
    let target = a.arg(0);
    let held = a.arg(1);
    let token = a.arg(2);
    if !can_be_held_weakly(vm, target) {
        return vm.type_error("FinalizationRegistry.prototype.register: invalid target");
    }
    if vm.same_value(target, held) {
        return vm.type_error("FinalizationRegistry.prototype.register: target and held value must differ");
    }
    if !can_be_held_weakly(vm, token) && !token.is_undefined() {
        return vm.type_error("FinalizationRegistry.prototype.register: invalid unregister token");
    }
    if let Class::FinalizationRegistry(f) = &mut vm.heap.obj_mut(o).class {
        f.cells.push(FinCell { target, held, token });
    }
    Ok(Value::Undefined)
}

fn finreg_unregister(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_finreg(vm, a.this, "unregister")?;
    let token = a.arg(0);
    if !can_be_held_weakly(vm, token) {
        return vm.type_error("FinalizationRegistry.prototype.unregister: invalid unregister token");
    }
    let mut removed = false;
    if let Class::FinalizationRegistry(f) = &mut vm.heap.obj_mut(o).class {
        let before = f.cells.len();
        f.cells.retain(|c| !same_identity(c.token, token));
        removed = f.cells.len() != before;
    }
    Ok(Value::Bool(removed))
}
