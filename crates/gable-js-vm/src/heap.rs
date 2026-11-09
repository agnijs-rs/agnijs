//! The heap: index-based arenas for every kind of heap value.
//!
//! Each arena is a `Vec` of slots with a free list and a mark bit per slot.
//! Ids are slot indices; a freed slot is reused by a later allocation, so a
//! stale id (one held past the collection that freed it) is a bug in the
//! engine, never in user code. Every access through [`Arena::get`] panics
//! on a dead slot rather than returning another value's data.
//!
//! Collection is precise mark-sweep, run only at interpreter safe points
//! (see [`crate::gc`]). Allocation never collects: it only records how
//! much was allocated so the interpreter can decide to collect later.
//! This module has no `unsafe` code.

use crate::bigint::BigIntData;
use crate::env::Environment;
use crate::object::{ObjectData, Shape};
use crate::string::{Repr, StringData, Units};
use crate::symbol::SymbolData;
use crate::value::{BigId, EnvId, ObjId, ShapeId, StrId, SymId};
use hashbrown::HashTable;

/// A slot arena with a free list and mark bits.
pub struct Arena<T> {
    slots: Vec<Option<T>>,
    marks: Vec<bool>,
    free: Vec<u32>,
    live: usize,
}

impl<T> Default for Arena<T> {
    fn default() -> Self {
        Arena {
            slots: Vec::new(),
            marks: Vec::new(),
            free: Vec::new(),
            live: 0,
        }
    }
}

impl<T> Arena<T> {
    pub fn alloc(&mut self, value: T) -> u32 {
        self.live += 1;
        if let Some(i) = self.free.pop() {
            self.slots[i as usize] = Some(value);
            self.marks[i as usize] = false;
            i
        } else {
            let i = self.slots.len() as u32;
            self.slots.push(Some(value));
            self.marks.push(false);
            i
        }
    }

    #[inline]
    pub fn get(&self, i: u32) -> &T {
        match self.slots.get(i as usize) {
            Some(Some(v)) => v,
            _ => panic!("heap: access to dead or invalid slot {i}"),
        }
    }

    #[inline]
    pub fn get_mut(&mut self, i: u32) -> &mut T {
        match self.slots.get_mut(i as usize) {
            Some(Some(v)) => v,
            _ => panic!("heap: access to dead or invalid slot {i}"),
        }
    }

    #[inline]
    pub fn try_get(&self, i: u32) -> Option<&T> {
        match self.slots.get(i as usize) {
            Some(Some(v)) => Some(v),
            _ => None,
        }
    }

    /// Two distinct slots mutably at once.
    pub fn get2_mut(&mut self, a: u32, b: u32) -> (&mut T, &mut T) {
        assert_ne!(a, b);
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        let (left, right) = self.slots.split_at_mut(hi as usize);
        let x = left[lo as usize].as_mut().expect("heap: dead slot");
        let y = right[0].as_mut().expect("heap: dead slot");
        if a < b {
            (x, y)
        } else {
            (y, x)
        }
    }

    /// Set the mark bit; true when the slot was not yet marked.
    #[inline]
    pub fn mark(&mut self, i: u32) -> bool {
        let m = &mut self.marks[i as usize];
        if *m {
            false
        } else {
            *m = true;
            true
        }
    }

    #[inline]
    pub fn is_marked(&self, i: u32) -> bool {
        self.marks[i as usize]
    }

    #[inline]
    pub fn is_live(&self, i: u32) -> bool {
        matches!(self.slots.get(i as usize), Some(Some(_)))
    }

    /// Free every unmarked live slot, clearing marks. `on_free` sees each
    /// freed value before it is dropped.
    pub fn sweep(&mut self, mut on_free: impl FnMut(u32, &T)) -> usize {
        let mut freed = 0;
        for i in 0..self.slots.len() {
            if self.slots[i].is_some() {
                if self.marks[i] {
                    self.marks[i] = false;
                } else {
                    if let Some(v) = &self.slots[i] {
                        on_free(i as u32, v);
                    }
                    self.slots[i] = None;
                    self.free.push(i as u32);
                    self.live -= 1;
                    freed += 1;
                }
            }
        }
        freed
    }

    pub fn live(&self) -> usize {
        self.live
    }

    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = (u32, &T)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.as_ref().map(|v| (i as u32, v)))
    }
}

/// Approximate allocation sizes, for GC scheduling only.
const OBJECT_BYTES: usize = 160;
const ENV_BYTES: usize = 64;
const SYMBOL_BYTES: usize = 32;
const STRING_BYTES: usize = 40;

pub struct Heap {
    pub objects: Arena<ObjectData>,
    pub strings: Arena<StringData>,
    pub symbols: Arena<SymbolData>,
    pub bigints: Arena<BigIntData>,
    pub envs: Arena<Environment>,
    /// Shapes are never freed in this version: they are bounded by the
    /// program's property-shape structure, and their keys are kept alive
    /// as roots. See ADR 0009.
    pub shapes: Vec<Shape>,
    /// Interned strings (atoms), weakly held: the sweep removes dead ones.
    intern: HashTable<StrId>,
    /// Bytes allocated since the last collection (approximate).
    pub allocated_since_gc: usize,
    /// Collect when `allocated_since_gc` passes this.
    pub gc_threshold: usize,
    /// Approximate live bytes after the last collection.
    pub live_bytes: usize,
    /// Stress mode (testing): collect at every safe point, or at every
    /// `stress_every`-th one. Set from `GABLE_GC_STRESS` (`1` or a count).
    pub stress: bool,
    pub stress_every: u32,
    pub stress_tick: u32,
    /// Optional memory limit; exceeding it after a collection is an error.
    pub memory_limit: Option<usize>,
    /// One-byte single-character strings, allocated on first use and kept
    /// alive as roots, so `s[i]`, `charAt` and `fromCharCode` do not
    /// allocate for Latin-1 characters.
    pub single_chars: Vec<Option<StrId>>,
}

impl Default for Heap {
    fn default() -> Self {
        Heap::new()
    }
}

impl Heap {
    pub fn new() -> Heap {
        let stress_every = match std::env::var("GABLE_GC_STRESS") {
            Ok(v) if !v.is_empty() && v != "0" => Some(v.parse::<u32>().unwrap_or(1).max(1)),
            _ => None,
        };
        Heap {
            objects: Arena::default(),
            strings: Arena::default(),
            symbols: Arena::default(),
            bigints: Arena::default(),
            envs: Arena::default(),
            shapes: Vec::new(),
            intern: HashTable::new(),
            allocated_since_gc: 0,
            gc_threshold: 8 * 1024 * 1024,
            live_bytes: 0,
            stress: stress_every.is_some(),
            stress_every: stress_every.unwrap_or(1),
            stress_tick: 0,
            memory_limit: None,
            single_chars: vec![None; 256],
        }
    }

    /// The string holding one UTF-16 code unit; cached for units below 256.
    pub fn char_str(&mut self, unit: u16) -> StrId {
        if unit < 256 {
            if let Some(s) = self.single_chars[unit as usize] {
                return s;
            }
            let s = self.alloc_str_latin1(vec![unit as u8]);
            self.single_chars[unit as usize] = Some(s);
            s
        } else {
            self.alloc_str_utf16(vec![unit])
        }
    }

    /// Whether a collection should run at the next safe point.
    #[inline]
    pub fn wants_gc(&mut self) -> bool {
        if self.stress {
            self.stress_tick += 1;
            if self.stress_tick >= self.stress_every {
                self.stress_tick = 0;
                return true;
            }
        }
        self.allocated_since_gc >= self.gc_threshold
    }

    // ---- objects -----------------------------------------------------------

    pub fn alloc_object(&mut self, data: ObjectData) -> ObjId {
        self.allocated_since_gc += OBJECT_BYTES;
        ObjId(self.objects.alloc(data))
    }

    #[inline]
    pub fn obj(&self, id: ObjId) -> &ObjectData {
        self.objects.get(id.0)
    }

    #[inline]
    pub fn obj_mut(&mut self, id: ObjId) -> &mut ObjectData {
        self.objects.get_mut(id.0)
    }

    // ---- environments ------------------------------------------------------

    pub fn alloc_env(&mut self, env: Environment) -> EnvId {
        self.allocated_since_gc += ENV_BYTES + env.slot_count() * 16;
        EnvId(self.envs.alloc(env))
    }

    #[inline]
    pub fn env(&self, id: EnvId) -> &Environment {
        self.envs.get(id.0)
    }

    #[inline]
    pub fn env_mut(&mut self, id: EnvId) -> &mut Environment {
        self.envs.get_mut(id.0)
    }

    // ---- symbols -----------------------------------------------------------

    pub fn alloc_symbol(&mut self, data: SymbolData) -> SymId {
        self.allocated_since_gc += SYMBOL_BYTES;
        SymId(self.symbols.alloc(data))
    }

    #[inline]
    pub fn sym(&self, id: SymId) -> &SymbolData {
        self.symbols.get(id.0)
    }

    #[inline]
    pub fn sym_mut(&mut self, id: SymId) -> &mut SymbolData {
        self.symbols.get_mut(id.0)
    }

    // ---- bigints -------------------------------------------------------------

    pub fn alloc_bigint(&mut self, data: BigIntData) -> BigId {
        self.allocated_since_gc += 32 + data.0.bits() as usize / 8;
        BigId(self.bigints.alloc(data))
    }

    #[inline]
    pub fn bigint(&self, id: BigId) -> &BigIntData {
        self.bigints.get(id.0)
    }

    // ---- shapes --------------------------------------------------------------

    pub fn alloc_shape(&mut self, shape: Shape) -> ShapeId {
        let id = ShapeId(self.shapes.len() as u32);
        self.shapes.push(shape);
        id
    }

    #[inline]
    pub fn shape(&self, id: ShapeId) -> &Shape {
        &self.shapes[id.index()]
    }

    #[inline]
    pub fn shape_mut(&mut self, id: ShapeId) -> &mut Shape {
        &mut self.shapes[id.index()]
    }

    // ---- strings -------------------------------------------------------------

    fn alloc_string(&mut self, data: StringData) -> StrId {
        self.allocated_since_gc += STRING_BYTES + data.byte_len();
        StrId(self.strings.alloc(data))
    }

    /// A string from UTF-16 code units; stored one-byte when possible.
    pub fn alloc_str_utf16(&mut self, units: Vec<u16>) -> StrId {
        if units.iter().all(|&u| u <= 0xFF) {
            let bytes: Vec<u8> = units.iter().map(|&u| u as u8).collect();
            self.alloc_string(StringData::new(Repr::One(bytes.into_boxed_slice())))
        } else {
            self.alloc_string(StringData::new(Repr::Two(units.into_boxed_slice())))
        }
    }

    /// A string from Latin-1 bytes.
    pub fn alloc_str_latin1(&mut self, bytes: Vec<u8>) -> StrId {
        self.alloc_string(StringData::new(Repr::One(bytes.into_boxed_slice())))
    }

    /// A string from Rust text.
    pub fn alloc_str(&mut self, s: &str) -> StrId {
        if s.is_ascii() {
            self.alloc_str_latin1(s.as_bytes().to_vec())
        } else {
            let units: Vec<u16> = s.encode_utf16().collect();
            self.alloc_str_utf16(units)
        }
    }

    pub fn alloc_str_units(&mut self, units: Units<'_>) -> StrId {
        match units {
            Units::One(b) => self.alloc_str_latin1(b.to_vec()),
            Units::Two(u) => self.alloc_str_utf16(u.to_vec()),
        }
    }

    #[inline]
    pub fn string(&self, id: StrId) -> &StringData {
        self.strings.get(id.0)
    }

    pub fn str_len(&self, id: StrId) -> usize {
        self.strings.get(id.0).len()
    }

    /// Concatenate two strings. Short results are copied; long ones become
    /// a rope flattened on first access.
    pub fn concat(&mut self, a: StrId, b: StrId) -> StrId {
        let la = self.str_len(a);
        let lb = self.str_len(b);
        if la == 0 {
            return b;
        }
        if lb == 0 {
            return a;
        }
        let total = la + lb;
        if total < 64 {
            self.flatten(a);
            self.flatten(b);
            let ua = self.strings.get(a.0).units();
            let ub = self.strings.get(b.0).units();
            if ua.is_one_byte() && ub.is_one_byte() {
                let mut v = Vec::with_capacity(total);
                v.extend_from_slice(ua.as_one_byte().unwrap());
                v.extend_from_slice(ub.as_one_byte().unwrap());
                return self.alloc_str_latin1(v);
            }
            let mut v = Vec::with_capacity(total);
            v.extend(ua.iter());
            v.extend(ub.iter());
            return self.alloc_string(StringData::new(Repr::Two(v.into_boxed_slice())));
        }
        let one_byte = self.strings.get(a.0).is_one_byte() && self.strings.get(b.0).is_one_byte();
        self.alloc_string(StringData::new(Repr::Rope {
            left: a,
            right: b,
            len: total as u32,
            one_byte,
        }))
    }

    /// Make the string flat (not a rope). Iterative, so deep ropes built by
    /// `s += x` loops do not overflow the stack.
    pub fn flatten(&mut self, id: StrId) {
        let (len, one_byte) = match &self.strings.get(id.0).repr {
            Repr::Rope { len, one_byte, .. } => (*len as usize, *one_byte),
            _ => return,
        };
        // Collect leaves left to right.
        let mut stack = vec![id];
        if one_byte {
            let mut out: Vec<u8> = Vec::with_capacity(len);
            while let Some(s) = stack.pop() {
                match &self.strings.get(s.0).repr {
                    Repr::Rope { left, right, .. } => {
                        stack.push(*right);
                        stack.push(*left);
                    }
                    Repr::One(b) => out.extend_from_slice(b),
                    Repr::Two(u) => out.extend(u.iter().map(|&x| x as u8)),
                }
            }
            self.strings.get_mut(id.0).repr = Repr::One(out.into_boxed_slice());
        } else {
            let mut out: Vec<u16> = Vec::with_capacity(len);
            while let Some(s) = stack.pop() {
                match &self.strings.get(s.0).repr {
                    Repr::Rope { left, right, .. } => {
                        stack.push(*right);
                        stack.push(*left);
                    }
                    Repr::One(b) => out.extend(b.iter().map(|&x| x as u16)),
                    Repr::Two(u) => out.extend_from_slice(u),
                }
            }
            self.strings.get_mut(id.0).repr = Repr::Two(out.into_boxed_slice());
        }
    }

    /// The code units of a string (flattening it first).
    #[inline]
    pub fn units(&mut self, id: StrId) -> Units<'_> {
        self.flatten(id);
        self.strings.get(id.0).units()
    }

    /// The code units of an already flat string.
    #[inline]
    pub fn units_flat(&self, id: StrId) -> Units<'_> {
        self.strings.get(id.0).units()
    }

    /// Content equality.
    pub fn str_eq(&mut self, a: StrId, b: StrId) -> bool {
        if a == b {
            return true;
        }
        if self.str_len(a) != self.str_len(b) {
            return false;
        }
        let sa = self.strings.get(a.0);
        let sb = self.strings.get(b.0);
        if sa.atom && sb.atom {
            return false;
        }
        self.flatten(a);
        self.flatten(b);
        let ua = self.strings.get(a.0).units();
        let ub = self.strings.get(b.0).units();
        ua.eq(&ub)
    }

    pub fn str_eq_ascii(&mut self, a: StrId, s: &str) -> bool {
        self.units(a).eq_str(s)
    }

    pub fn str_hash(&mut self, id: StrId) -> u64 {
        if let Some(h) = self.strings.get(id.0).cached_hash() {
            return h;
        }
        self.flatten(id);
        let h = self.strings.get(id.0).units().hash();
        self.strings.get_mut(id.0).set_hash(h);
        h
    }

    /// The canonical atom with this string's content.
    pub fn intern(&mut self, id: StrId) -> StrId {
        if self.strings.get(id.0).atom {
            return id;
        }
        let hash = self.str_hash(id);
        self.flatten(id);
        let strings = &self.strings;
        let needle = strings.get(id.0).units();
        if let Some(&found) = self.intern.find(hash, |&cand| {
            let c = strings.get(cand.0);
            c.len() == needle.len() && c.units().eq(&needle)
        }) {
            return found;
        }
        self.strings.get_mut(id.0).atom = true;
        let strings = &self.strings;
        self.intern.insert_unique(hash, id, |&s| {
            strings.get(s.0).cached_hash().expect("atom without hash")
        });
        id
    }

    /// The atom for Rust text.
    pub fn atom(&mut self, s: &str) -> StrId {
        let hash = Units::hash_str(s);
        let strings = &self.strings;
        if let Some(&found) = self.intern.find(hash, |&cand| {
            let c = strings.get(cand.0);
            c.units().eq_str(s)
        }) {
            return found;
        }
        let id = self.alloc_str(s);
        self.strings.get_mut(id.0).set_hash(hash);
        self.strings.get_mut(id.0).atom = true;
        let strings = &self.strings;
        self.intern.insert_unique(hash, id, |&x| {
            strings.get(x.0).cached_hash().expect("atom without hash")
        });
        id
    }

    /// The atom for UTF-16 content.
    pub fn atom_utf16(&mut self, units: &[u16]) -> StrId {
        let hash = Units::Two(units).hash();
        let strings = &self.strings;
        if let Some(&found) = self.intern.find(hash, |&cand| {
            let c = strings.get(cand.0);
            c.len() == units.len() && c.units().eq(&Units::Two(units))
        }) {
            return found;
        }
        let id = self.alloc_str_utf16(units.to_vec());
        self.strings.get_mut(id.0).set_hash(hash);
        self.strings.get_mut(id.0).atom = true;
        let strings = &self.strings;
        self.intern.insert_unique(hash, id, |&x| {
            strings.get(x.0).cached_hash().expect("atom without hash")
        });
        id
    }

    /// Remove dead atoms from the intern table (called by the sweep,
    /// before the string arena is swept).
    pub(crate) fn prune_intern_table(&mut self) {
        let strings = &self.strings;
        self.intern.retain(|&mut id| strings.is_marked(id.0));
    }

    /// Lossy Rust string of a JS string (for diagnostics and host output).
    pub fn str_to_string(&mut self, id: StrId) -> String {
        self.units(id).to_string_lossy()
    }

    /// Approximate memory used by live values.
    pub fn approx_live_bytes(&self) -> usize {
        let mut bytes = self.objects.live() * OBJECT_BYTES
            + self.envs.live() * ENV_BYTES
            + self.symbols.live() * SYMBOL_BYTES
            + self.bigints.live() * 48;
        for (_, s) in self.strings.iter() {
            bytes += STRING_BYTES + s.byte_len();
        }
        for (_, o) in self.objects.iter() {
            bytes += o.extra_bytes();
        }
        for (_, e) in self.envs.iter() {
            bytes += e.slot_count() * 16;
        }
        bytes
    }
}
