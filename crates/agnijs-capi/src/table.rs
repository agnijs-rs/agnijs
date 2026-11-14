//! The per-engine table behind value and buffer ids.
//!
//! An id is 64 bits: `tag:20 | generation:16 | index:28`. The tag names the
//! engine (so an id given to the wrong engine is caught) and no two live
//! engines share one (`crate::registry`, TASK-103.40). The generation
//! changes every time a slot is freed (so a freed or stale id is caught),
//! and a slot whose generation would wrap is retired instead of reused, so
//! an old id can never alias a new value. Generation 0 marks realm ids and
//! 0xFFFF class ids, so no value id is ever mistaken for one of those.
//!
//! The layout is private: the header only promises an opaque nonzero
//! `uint64_t`, so changing it is not an ABI change.

use crate::guard::{Fail, Res};
use crate::types::*;
use agnijs_engine_api as api;

/// Generations a value slot cycles through before it retires.
const GEN_FIRST: u16 = 1;
const GEN_LAST: u16 = 0xFFFE;
pub(crate) const GEN_REALM: u16 = 0;
pub(crate) const GEN_CLASS: u16 = 0xFFFF;
/// The largest engine tag; tags run from 1 to this.
pub(crate) const TAG_MAX: u32 = (1 << 20) - 1;
/// The largest slot index an id can carry.
pub(crate) const INDEX_MAX: u32 = (1 << 28) - 1;
/// The most slots one engine's table may hold.
const MAX_SLOTS: usize = 1 << 28;

/// What an id refers to.
pub enum Entry {
    Value(api::Value),
    Bytes(Box<[u8]>),
}

struct Slot {
    generation: u16,
    entry: Option<Entry>,
}

pub struct Table {
    tag: u32,
    slots: Vec<Slot>,
    free: Vec<u32>,
    live: usize,
}

/// Split an id into (tag, generation, index).
pub fn split(id: u64) -> (u32, u16, u32) {
    let tag = u32::try_from(id >> 44).unwrap_or(0);
    let generation = u16::try_from((id >> 28) & 0xFFFF).unwrap_or(0);
    let index = u32::try_from(id & u64::from(INDEX_MAX)).unwrap_or(0);
    (tag, generation, index)
}

/// Build an id. Each part is masked to its field, so no part can spill
/// into another; callers keep `tag <= TAG_MAX` and `index <= INDEX_MAX`.
pub fn join(tag: u32, generation: u16, index: u32) -> u64 {
    (u64::from(tag & TAG_MAX) << 44) | (u64::from(generation) << 28) | u64::from(index & INDEX_MAX)
}

impl Table {
    pub fn new(tag: u32) -> Table {
        Table { tag, slots: Vec::new(), free: Vec::new(), live: 0 }
    }

    pub fn live(&self) -> usize {
        self.live
    }

    pub fn insert(&mut self, entry: Entry) -> Res<u64> {
        let index = match self.free.pop() {
            Some(i) => i,
            None => {
                if self.slots.len() >= MAX_SLOTS {
                    return Err(Fail::new(AGNIJS_ERR_LIMIT, "the engine's handle table is full; free some values"));
                }
                self.slots.push(Slot { generation: GEN_FIRST, entry: None });
                u32::try_from(self.slots.len().saturating_sub(1))
                    .map_err(|_| Fail::new(AGNIJS_ERR_LIMIT, "handle table index overflow"))?
            }
        };
        let slot = self
            .slots
            .get_mut(usize::try_from(index).unwrap_or(usize::MAX))
            .ok_or_else(|| Fail::new(AGNIJS_ERR_ENGINE, "handle table free list is inconsistent"))?;
        slot.entry = Some(entry);
        self.live = self.live.saturating_add(1);
        Ok(join(self.tag, slot.generation, index))
    }

    fn check(&self, id: u64, what: &str) -> Res<usize> {
        if id == 0 {
            return Err(Fail::new(AGNIJS_ERR_INVALID_HANDLE, format!("{what} is the null handle (0)")));
        }
        let (tag, generation, index) = split(id);
        if tag != self.tag {
            return Err(Fail::new(AGNIJS_ERR_WRONG_ENGINE, format!("{what} belongs to another engine")));
        }
        if generation == GEN_REALM || generation == GEN_CLASS {
            return Err(Fail::new(AGNIJS_ERR_INVALID_HANDLE, format!("{what} is a realm or class id, not a value")));
        }
        let i = usize::try_from(index).unwrap_or(usize::MAX);
        match self.slots.get(i) {
            Some(slot) if slot.generation == generation && slot.entry.is_some() => Ok(i),
            Some(_) => Err(Fail::new(AGNIJS_ERR_INVALID_HANDLE, format!("{what} was freed (stale handle)"))),
            None => Err(Fail::new(AGNIJS_ERR_INVALID_HANDLE, format!("{what} is not a handle of this engine"))),
        }
    }

    pub fn get(&self, id: u64, what: &str) -> Res<&Entry> {
        let i = self.check(id, what)?;
        self.slots
            .get(i)
            .and_then(|s| s.entry.as_ref())
            .ok_or_else(|| Fail::new(AGNIJS_ERR_INVALID_HANDLE, format!("{what} was freed")))
    }

    pub fn value(&self, id: u64, what: &str) -> Res<api::Value> {
        match self.get(id, what)? {
            Entry::Value(v) => Ok(v.clone()),
            Entry::Bytes(_) => Err(Fail::new(AGNIJS_ERR_INVALID_HANDLE, format!("{what} is a buffer, not a value"))),
        }
    }

    pub fn bytes(&self, id: u64, what: &str) -> Res<&[u8]> {
        match self.get(id, what)? {
            Entry::Bytes(b) => Ok(b),
            Entry::Value(_) => Err(Fail::new(AGNIJS_ERR_INVALID_HANDLE, format!("{what} is a value, not a buffer"))),
        }
    }

    /// Free an id and return what it held. A second free of the same id is
    /// an error, never a double free.
    pub fn remove(&mut self, id: u64, what: &str) -> Res<Entry> {
        let i = self.check(id, what)?;
        let slot = self
            .slots
            .get_mut(i)
            .ok_or_else(|| Fail::new(AGNIJS_ERR_INVALID_HANDLE, format!("{what} was freed")))?;
        let entry = slot
            .entry
            .take()
            .ok_or_else(|| Fail::new(AGNIJS_ERR_INVALID_HANDLE, format!("{what} was freed")))?;
        self.live = self.live.saturating_sub(1);
        if slot.generation < GEN_LAST {
            slot.generation = slot.generation.saturating_add(1);
            if let Ok(i) = u32::try_from(i) {
                self.free.push(i);
            }
        } else {
            // Retired: its generations are used up.
            slot.generation = GEN_CLASS;
        }
        Ok(entry)
    }

    /// Free, accepting the kind expected.
    pub fn remove_kind(&mut self, id: u64, want_bytes: bool, what: &str) -> Res<Entry> {
        match (self.get(id, what)?, want_bytes) {
            (Entry::Value(_), true) => {
                Err(Fail::new(AGNIJS_ERR_INVALID_HANDLE, format!("{what} is a value; free it with agnijs_value_free")))
            }
            (Entry::Bytes(_), false) => Err(Fail::new(
                AGNIJS_ERR_INVALID_HANDLE,
                format!("{what} is a buffer; free it with agnijs_buffer_free"),
            )),
            _ => self.remove(id, what),
        }
    }

    /// Drop everything (engine teardown).
    pub fn clear(&mut self) -> Vec<Entry> {
        self.live = 0;
        self.free.clear();
        self.slots.drain(..).filter_map(|s| s.entry).collect()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn miri_freed_ids_are_stale_and_slots_retire_instead_of_wrapping() {
        let mut t = Table::new(7);
        let a = t.insert(Entry::Value(api::Value::Number(1.0))).unwrap();
        assert!(t.remove(a, "v").is_ok());
        assert_eq!(t.remove(a, "v").err().unwrap().status, AGNIJS_ERR_INVALID_HANDLE);
        let b = t.insert(Entry::Value(api::Value::Null)).unwrap();
        assert_ne!(a, b, "a reused slot gets a new generation");
        assert_eq!(t.value(a, "v").err().unwrap().status, AGNIJS_ERR_INVALID_HANDLE);
        // Drive one slot through all its generations.
        let mut id = b;
        for _ in 0..(GEN_LAST as usize) {
            t.remove(id, "v").unwrap();
            id = t.insert(Entry::Value(api::Value::Null)).unwrap();
        }
        let (_, _, index) = split(id);
        assert_ne!(index, split(b).2, "the exhausted slot was retired");
    }

    #[test]
    fn miri_ids_from_other_tables_and_kinds_are_refused() {
        let mut t = Table::new(1);
        let mut u = Table::new(2);
        let a = t.insert(Entry::Bytes(Box::new([1, 2]))).unwrap();
        assert_eq!(u.value(a, "v").err().unwrap().status, AGNIJS_ERR_WRONG_ENGINE);
        assert_eq!(t.value(a, "v").err().unwrap().status, AGNIJS_ERR_INVALID_HANDLE);
        assert_eq!(t.remove_kind(a, false, "v").err().unwrap().status, AGNIJS_ERR_INVALID_HANDLE);
        assert_eq!(t.value(0, "v").err().unwrap().status, AGNIJS_ERR_INVALID_HANDLE);
        assert_eq!(t.value(u64::MAX, "v").err().unwrap().status, AGNIJS_ERR_WRONG_ENGINE);
        assert_eq!(t.value(join(1, GEN_REALM, 1), "v").err().unwrap().status, AGNIJS_ERR_INVALID_HANDLE);
        assert_eq!(t.value(join(1, 1, 999), "v").err().unwrap().status, AGNIJS_ERR_INVALID_HANDLE);
        assert_eq!(u.insert(Entry::Value(api::Value::Null)).map(|_| ()).err().map(|f| f.status), None);
    }
}
