//! Map, Set, WeakMap, WeakSet, WeakRef and FinalizationRegistry: the data
//! model and weak-reference processing. The constructors land in M3.

use crate::gc::Tracer;
use crate::value::{ObjId, Value};
use crate::vm::Vm;

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
    pub fn trace(&self, t: &mut Tracer) {
        for e in self.entries.iter().flatten() {
            if !self.weak {
                t.value(e.0);
            }
            t.value(e.1);
        }
    }
}

pub struct MapIterState {
    pub target: Option<ObjId>,
    pub index: usize,
    pub kind: crate::object::IterKind,
}

pub struct FinRegData {
    pub cleanup: Value,
    pub cells: Vec<FinCell>,
}

pub struct FinCell {
    pub target: Value,
    pub held: Value,
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

/// Clear weak references whose targets died in this collection.
pub fn process_weak(_vm: &mut Vm) {
    // M3: WeakMap/WeakSet entry removal, WeakRef clearing and
    // FinalizationRegistry cleanup scheduling.
}

pub fn install(_vm: &mut Vm, _realm: gable_engine_api::RealmId) {}
