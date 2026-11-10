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
use crate::object::{FunctionData, KeyTable, ObjectData, Shape};

use crate::string::{Repr, StringData, Units};
use crate::symbol::SymbolData;
use crate::object::KeyTableId;
use crate::value::{BigId, EnvId, ObjId, PropSlot, ShapeId, StrId, SymId, Value};
use hashbrown::HashTable;
use std::collections::HashSet;

/// A slot arena with a free list, mark bits and a birth serial per slot
/// (the value of [`Heap::alloc_serial`] when the slot was last allocated;
/// see `gc.rs`, retention under native frames).
pub struct Arena<T> {
    slots: Vec<Option<T>>,
    marks: Vec<bool>,
    births: Vec<u64>,
    free: Vec<u32>,
    live: usize,
}

impl<T> Default for Arena<T> {
    fn default() -> Self {
        Arena {
            slots: Vec::new(),
            marks: Vec::new(),
            births: Vec::new(),
            free: Vec::new(),
            live: 0,
        }
    }
}

impl<T> Arena<T> {
    pub fn alloc(&mut self, value: T, serial: u64) -> u32 {
        self.live += 1;
        if let Some(i) = self.free.pop() {
            self.slots[i as usize] = Some(value);
            self.marks[i as usize] = false;
            self.births[i as usize] = serial;
            i
        } else {
            let i = self.slots.len() as u32;
            self.slots.push(Some(value));
            self.marks.push(false);
            self.births.push(serial);
            i
        }
    }

    /// The live slots born before `serial` (allocated while
    /// `alloc_serial < serial`). Scans only the births: a free slot's is
    /// `u64::MAX`.
    pub(crate) fn born_before(&self, serial: u64) -> impl Iterator<Item = u32> + '_ {
        self.births
            .iter()
            .enumerate()
            .filter(move |(_, &b)| b < serial)
            .map(|(i, _)| i as u32)
    }

    /// The slot array's base, for compiled code (slot `i` is
    /// `size_of::<Option<T>>()` bytes times `i` from it). Valid until the
    /// arena is next used from Rust.
    #[cfg(feature = "jit")]
    pub(crate) fn base_ptr(&mut self) -> *mut u8 {
        self.slots.as_mut_ptr() as *mut u8
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

    /// Free every unmarked live slot, clearing marks. `live` sees each
    /// survivor; `dead` takes each freed value (and may keep its buffers).
    pub fn sweep(&mut self, mut live: impl FnMut(&T), mut dead: impl FnMut(T)) -> usize {
        let mut freed = 0;
        for (i, (slot, mark)) in self.slots.iter_mut().zip(self.marks.iter_mut()).enumerate() {
            if *mark {
                *mark = false;
                if let Some(v) = slot {
                    live(v);
                }
            } else if let Some(v) = slot.take() {
                dead(v);
                self.births[i] = u64::MAX;
                self.free.push(i as u32);
                freed += 1;
            }
        }
        self.live -= freed;
        freed
    }

    /// Move the mark bits out for the marking phase (see `gc::Tracer`);
    /// [`Arena::restore_marks`] puts them back. Every slot is unmarked
    /// between collections.
    pub(crate) fn take_marks(&mut self) -> Vec<bool> {
        std::mem::take(&mut self.marks)
    }

    pub(crate) fn restore_marks(&mut self, marks: Vec<bool>) {
        debug_assert_eq!(marks.len(), self.slots.len());
        self.marks = marks;
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

/// Storage buffers taken from dead heap values by the sweep and handed to
/// new ones (TASK-100.22), so a collection that frees many small objects
/// does not pay one allocator call per buffer, and the program that
/// replaces them does not pay another. Buffers are kept empty. Only
/// buffers of at most `MAX_CAP` entries are kept, up to a byte limit the
/// collector sets from its threshold (what the program allocates before
/// the next collection); the rest are freed as before, and so are
/// buffers still unused when the next collection starts.
pub struct BufPool<T> {
    bufs: Vec<Vec<T>>,
    bytes: usize,
    limit: usize,
}

impl<T> Default for BufPool<T> {
    fn default() -> Self {
        BufPool {
            bufs: Vec::new(),
            bytes: 0,
            limit: Self::MIN_LIMIT,
        }
    }
}

impl<T> BufPool<T> {
    const MAX_CAP: usize = 32;
    const MIN_LIMIT: usize = 4 * 1024 * 1024;
    const MAX_LIMIT: usize = 64 * 1024 * 1024;

    /// Start a sweep: free the buffers no allocation needed since the last
    /// one, and keep up to about `budget` bytes from this one.
    pub fn restart(&mut self, budget: usize) {
        self.bufs.clear();
        self.bytes = 0;
        self.limit = budget.clamp(Self::MIN_LIMIT, Self::MAX_LIMIT);
    }


    /// Keep `buf`'s storage for reuse, or free it.
    #[inline]
    pub fn give(&mut self, mut buf: Vec<T>) {
        let cap = buf.capacity();
        if cap == 0 || cap > Self::MAX_CAP {
            return;
        }
        let bytes = cap * std::mem::size_of::<T>();
        if self.bytes + bytes > self.limit {
            return;
        }
        buf.clear();
        self.bytes += bytes;
        self.bufs.push(buf);
    }

    /// An empty buffer, with capacity when the pool has one.
    #[inline]
    pub fn take(&mut self) -> Vec<T> {
        match self.bufs.pop() {
            Some(b) => {
                self.bytes -= b.capacity() * std::mem::size_of::<T>();
                b
            }
            None => Vec::new(),
        }
    }

    /// Bytes held for reuse.
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

/// Approximate allocation sizes, for GC scheduling only. The fixed part of
/// each heap value is counted when it is allocated; the storage that grows
/// afterwards (elements, property slots, dictionaries, flattened string
/// buffers) is counted as it grows, through [`Heap::note_growth`], at the
/// per-entry sizes below. [`Heap::approx_live_bytes`] uses the same sizes,
/// so the live figure after a collection and the allocation counter agree.
/// `ObjectData`, inline slots included.
pub(crate) const OBJECT_BYTES: usize = 160;
pub(crate) const ENV_BYTES: usize = 64;
pub(crate) const SYMBOL_BYTES: usize = 32;
pub(crate) const STRING_BYTES: usize = 40;
/// The floor of the collection threshold: the least allocation between two
/// collections, which bounds the garbage a small heap holds. 2 MiB: a
/// collection with a small live set marks in about 0.2 ms, and the Gable
/// benches with a live set under 1 MB peaked at three to five times
/// QuickJS-NG's memory with the earlier 8 MiB floor (TASK-100.35).
/// Overridable for pacing experiments through `AGNIJS_GC_THRESHOLD`
/// (bytes), read once per process.
const DEFAULT_GC_THRESHOLD: usize = 2 * 1024 * 1024;

/// The next threshold as a percentage of the live bytes after a collection
/// (the growth factor). Overridable through `AGNIJS_GC_GROWTH` (percent).
const DEFAULT_GC_GROWTH_PERCENT: usize = 200;

fn pacing() -> (usize, usize) {
    static PACING: std::sync::OnceLock<(usize, usize)> = std::sync::OnceLock::new();
    *PACING.get_or_init(|| {
        let env = |name: &str, min: usize, default: usize| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|&v| v >= min)
                .unwrap_or(default)
        };
        (
            env("AGNIJS_GC_THRESHOLD", 64 * 1024, DEFAULT_GC_THRESHOLD),
            env("AGNIJS_GC_GROWTH", 110, DEFAULT_GC_GROWTH_PERCENT),
        )
    })
}

fn threshold_floor() -> usize {
    pacing().0
}

/// Attention bit: a collection is wanted at the next safe point (threshold
/// or memory limit reached, requested by the host, or stress mode).
pub const ATTENTION_GC: u32 = 1;
/// Attention bit: a debugger client is attached, so every instruction is a
/// potential pause.
pub const ATTENTION_DEBUG: u32 = 2;
/// Attention bit: a fuel budget is set (`Vm::set_fuel`), so every
/// instruction is charged one unit at the loop head.
pub const ATTENTION_FUEL: u32 = 4;
/// One dense element (a `Value`).
pub(crate) const ELEMENT_BYTES: usize = 16;
/// One shape-described property slot past the inline ones (a `PropSlot`).
pub(crate) const SLOT_BYTES: usize = 16;
/// One dictionary-mode property, or one sparse element.
pub(crate) const DICT_ENTRY_BYTES: usize = 48;

pub struct Heap {
    pub objects: Arena<ObjectData>,
    pub strings: Arena<StringData>,
    pub symbols: Arena<SymbolData>,
    pub bigints: Arena<BigIntData>,
    pub envs: Arena<Environment>,
    /// Shapes, by id; shape 0 is the empty root shape. A shape lives while
    /// a live object has it, a live shape descends from it, or a traced
    /// inline cache names it (`gc::Tracer::shape`); the collector frees the
    /// rest and reuses their ids. Inline caches that the collector did not
    /// trace are invalidated through `shape_epoch` before they can hit on a
    /// reused id. See ADR 0009.
    pub shapes: Vec<Shape>,
    /// Freed shape ids, for reuse.
    shape_free: Vec<u32>,
    /// The key storage shapes share along a transition chain
    /// (`object::KeyTable`); table 0 is the root shape's empty table.
    /// Freed with the last shape over them.
    pub key_tables: Vec<KeyTable>,
    /// Freed key-table ids, for reuse.
    table_free: Vec<u32>,
    /// Moves whenever a collection frees a shape. Inline-cache entries
    /// (`Feedback::shape_epoch`) recorded in an earlier epoch may name a
    /// freed, reused shape id and are cleared before use, unless the
    /// collector traced them (so every shape they name survived) and
    /// carried them over.
    pub shape_epoch: u64,
    /// Emptied property-slot buffers of dead objects, for new objects.
    pub(crate) slot_pool: BufPool<PropSlot>,
    /// Emptied value buffers of dead objects' elements and dead scopes'
    /// bindings, for new ones.
    pub(crate) value_pool: BufPool<Value>,
    /// Dead functions' data boxes, for new functions (no more than the
    /// objects that fit under the collection threshold; emptied at the
    /// start of each sweep, so a stale box keeps its old code block alive
    /// until the next collection at most).
    pub(crate) fn_pool: Vec<Box<FunctionData>>,
    /// Interned strings (atoms), weakly held: the sweep removes dead ones.
    intern: HashTable<StrId>,
    /// The VM's attention word (ADR 0019 J7): non-zero when the dispatch
    /// loop must leave its fast path before the next instruction, for a
    /// collection ([`ATTENTION_GC`]: the threshold or the memory limit was
    /// reached, the host or `$262.gc()` asked, or stress mode is on) or for
    /// the debugger ([`ATTENTION_DEBUG`]: a client is attached) or for fuel
    /// ([`ATTENTION_FUEL`]: a budget is set). It lives in
    /// the heap because the allocator is its most frequent setter; it is one
    /// word at a fixed offset of `Vm` all the same. `Vm::refresh_attention`
    /// recomputes it from its sources.
    pub attention: u32,
    /// Bytes allocated since the last collection (approximate). Grown only
    /// through [`Heap::charge`], which raises the attention word when the
    /// threshold is reached.
    pub allocated_since_gc: usize,
    /// Collect when `allocated_since_gc` passes this: the live bytes after
    /// the last collection times the growth factor (twice, by default),
    /// at least the floor (2 MiB), and never more than the distance to the
    /// memory limit, so a limit is noticed as soon as it is reached
    /// (`rearm_threshold`).
    pub gc_threshold: usize,
    /// Approximate live bytes after the last collection.
    pub live_bytes: usize,
    /// Stress mode (testing): collect at every safe point, or at every
    /// `stress_every`-th one. Set from `AGNIJS_GC_STRESS` (`1` or a count).
    pub stress: bool,
    pub stress_every: u32,
    pub stress_tick: u32,
    /// Optional memory limit; exceeding it after a collection is an error.
    pub memory_limit: Option<usize>,
    /// One-byte single-character strings, allocated on first use and kept
    /// alive as roots, so `s[i]`, `charAt` and `fromCharCode` do not
    /// allocate for Latin-1 characters.
    pub single_chars: Vec<Option<StrId>>,
    /// The prototype epoch (TASK-100.20): inline-cache entries for a
    /// property found on a prototype hold while it is unchanged. It moves
    /// when a *watched* object (one some entry looked through, see
    /// `ObjectData::watched`) changes its keys, attributes or prototype, or
    /// dies. It saturates; once there, no prototype entry is recorded.
    pub proto_epoch: u32,
    /// Atoms pinned for the property keys of code-block names, kept alive
    /// as roots so the keys can be computed once per block (TASK-100.20).
    pub pinned: HashSet<StrId>,
    /// Strings allocated so far (diagnostics and tests).
    pub strings_allocated: u64,
    /// Objects allocated so far (diagnostics and tests).
    pub objects_allocated: u64,
    /// The threshold `rearm_threshold` set; `gc_threshold` is raised above
    /// it while a collection is put off inside a nested run loop
    /// (`Vm::maybe_collect`) and restored when that loop ends.
    pub gc_threshold_base: usize,
    /// Heap values allocated so far, over every arena; the next slot's
    /// birth serial. Starts at 1 so that a retention bound of 0 (see
    /// `Vm::retain_from`) retains nothing.
    pub alloc_serial: u64,
    /// The value of `alloc_serial` at the last collection.
    pub serial_at_gc: u64,
    /// Bytes the last collection kept only for the native frames then on
    /// the stack (unreachable from the roots, born before the innermost
    /// nested run loop's entry; see `gc.rs`). Garbage until a collection
    /// at an outer level frees it; left out of the pacing basis.
    pub retained_bytes: usize,
}

impl Default for Heap {
    fn default() -> Self {
        Heap::new()
    }
}

impl Heap {
    pub fn new() -> Heap {
        let stress_every = match std::env::var("AGNIJS_GC_STRESS") {
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
            shape_free: Vec::new(),
            key_tables: vec![KeyTable::default()],
            table_free: Vec::new(),
            shape_epoch: 0,
            slot_pool: BufPool::default(),
            value_pool: BufPool::default(),
            fn_pool: Vec::new(),
            intern: HashTable::new(),
            attention: if stress_every.is_some() { ATTENTION_GC } else { 0 },
            allocated_since_gc: 0,
            gc_threshold: threshold_floor(),
            gc_threshold_base: threshold_floor(),
            live_bytes: 0,
            stress: stress_every.is_some(),
            stress_every: stress_every.unwrap_or(1),
            stress_tick: 0,
            memory_limit: None,
            single_chars: vec![None; 256],
            proto_epoch: 1,
            pinned: HashSet::new(),
            strings_allocated: 0,
            objects_allocated: 0,
            alloc_serial: 1,
            serial_at_gc: 0,
            retained_bytes: 0,
        }
    }

    /// Invalidate every prototype inline-cache entry.
    #[inline]
    pub fn bump_proto_epoch(&mut self) {
        self.proto_epoch = self.proto_epoch.saturating_add(1);
    }

    /// Note a change to `obj`'s keys, attributes or prototype.
    #[inline]
    pub fn note_structure_change(&mut self, obj: ObjId) {
        if self.obj(obj).watched {
            self.bump_proto_epoch();
        }
    }

    /// Set `obj`'s prototype (the only way prototypes change after
    /// creation, so inline caches see it).
    pub fn set_proto(&mut self, obj: ObjId, proto: Option<ObjId>) {
        let o = self.obj_mut(obj);
        if o.proto() != proto {
            o.set_proto_raw(proto);
            self.note_structure_change(obj);
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

    /// Whether a collection is worth running at a free safe point: one
    /// where no native frame is on the stack (a host call's entry, the
    /// boundary between jobs), so that it retains nothing. Garbage left
    /// since the last collection is kept by every nested run loop entered
    /// after it, until a collection at a free point frees it, so such a
    /// point collects once half the threshold has accumulated: that bounds
    /// what the next loop can retain to half a threshold, at the cost of
    /// at most one extra collection per threshold (TASK-100.35).
    #[inline]
    pub fn wants_free_gc(&self) -> bool {
        self.allocated_since_gc * 2 >= self.gc_threshold_base
    }

    /// Count `bytes` of storage that grew inside an existing heap value
    /// (elements, slots, a dictionary, a flattened string) towards the next
    /// collection and the memory limit.
    #[inline]
    pub fn note_growth(&mut self, bytes: usize) {
        self.charge(bytes);
    }

    /// Count `bytes` of new storage towards the next collection, and ask
    /// the dispatch loop for a safe point once the threshold is reached.
    #[inline]
    pub fn charge(&mut self, bytes: usize) {
        self.allocated_since_gc += bytes;
        if self.allocated_since_gc >= self.gc_threshold {
            self.attention |= ATTENTION_GC;
        }
    }

    /// Set the threshold for the next collection from the live bytes of the
    /// last one (less what it kept only for native frames) and the memory
    /// limit; called after a collection and when the limit changes.
    pub fn rearm_threshold(&mut self) {
        let basis = self.live_bytes.saturating_sub(self.retained_bytes);
        let (floor, growth) = pacing();
        let mut t = (basis / 100 * growth).max(floor);
        if let Some(limit) = self.memory_limit {
            // The limit is exceeded when live + allocated > limit.
            t = t.min(limit.saturating_sub(self.live_bytes) + 1);
        }
        self.gc_threshold = t;
        self.gc_threshold_base = t;
        if self.allocated_since_gc >= t {
            self.attention |= ATTENTION_GC;
        }
    }

    pub fn set_memory_limit(&mut self, bytes: Option<usize>) {
        self.memory_limit = bytes;
        self.rearm_threshold();
    }

    // ---- objects -----------------------------------------------------------

    /// An object arrives with whatever elements and slots its creator
    /// filled in (array literals, `new_array`), so that storage counts here.
    pub fn alloc_object(&mut self, data: ObjectData) -> ObjId {
        self.charge(OBJECT_BYTES + data.extra_bytes());
        self.objects_allocated += 1;
        let s = self.next_serial();
        ObjId(self.objects.alloc(data, s))
    }

    #[inline]
    fn next_serial(&mut self) -> u64 {
        let s = self.alloc_serial;
        self.alloc_serial += 1;
        s
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
        self.charge(ENV_BYTES + env.slot_count() * 16);
        let s = self.next_serial();
        EnvId(self.envs.alloc(env, s))
    }

    /// A declarative environment for `scope` (see
    /// [`Environment::declarative`]), its bindings in a recycled buffer
    /// when the sweep left one.
    pub fn alloc_declarative(
        &mut self,
        scope: Option<(std::rc::Rc<agnijs_js_compiler::bytecode::CodeBlock>, u32)>,
        parent: Option<EnvId>,
    ) -> EnvId {
        let has_bindings = scope
            .as_ref()
            .is_some_and(|(code, idx)| !code.scopes[*idx as usize].bindings.is_empty());
        let buf = if has_bindings {
            self.value_pool.take()
        } else {
            Vec::new()
        };
        self.alloc_env(Environment::declarative_in(buf, scope, parent))
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
        self.charge(SYMBOL_BYTES);
        let s = self.next_serial();
        SymId(self.symbols.alloc(data, s))
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
        self.charge(32 + data.0.bits() as usize / 8);
        let s = self.next_serial();
        BigId(self.bigints.alloc(data, s))
    }

    #[inline]
    pub fn bigint(&self, id: BigId) -> &BigIntData {
        self.bigints.get(id.0)
    }

    // ---- shapes --------------------------------------------------------------

    pub fn alloc_shape(&mut self, shape: Shape) -> ShapeId {
        if let Some(i) = self.shape_free.pop() {
            self.shapes[i as usize] = shape;
            return ShapeId(i);
        }
        let id = ShapeId(self.shapes.len() as u32);
        self.shapes.push(shape);
        id
    }

    pub(crate) fn alloc_key_table(&mut self, table: KeyTable) -> KeyTableId {
        if let Some(i) = self.table_free.pop() {
            self.key_tables[i as usize] = table;
            return KeyTableId(i);
        }
        let id = KeyTableId(self.key_tables.len() as u32);
        self.key_tables.push(table);
        id
    }

    /// Shapes in use (for tests and diagnostics).
    pub fn live_shapes(&self) -> usize {
        self.shapes.len() - self.shape_free.len()
    }

    /// Key tables in use (for tests and diagnostics).
    pub fn live_key_tables(&self) -> usize {
        self.key_tables.len() - self.table_free.len()
    }

    /// Free every shape the collector did not mark (`marks`, by shape id;
    /// the root is always marked) and every key table no surviving shape
    /// uses; trim each surviving table to the longest surviving shape over
    /// it (`table_len`, by table id). Dead shapes leave their live parents'
    /// transition maps. Returns the number of shapes freed; when it is not
    /// zero the shape epoch moves (the caller re-stamps the inline caches
    /// it traced).
    pub(crate) fn sweep_shapes(&mut self, marks: &[bool], table_len: &[u32]) -> usize {
        debug_assert!(marks[0], "the root shape is always live");
        let mut freed = 0;
        for i in 1..self.shapes.len() {
            if marks[i] || !self.shapes[i].live {
                continue;
            }
            let (parent, table, len) = {
                let s = &self.shapes[i];
                (s.parent, s.table, s.len)
            };
            if let Some(p) = parent {
                if marks[p.index()] {
                    let key = self.key_tables[table.0 as usize].keys[len as usize - 1];
                    self.shapes[p.index()].unlink(key, ShapeId(i as u32));
                }
            }
            self.shapes[i] = Shape::dead();
            self.shape_free.push(i as u32);
            freed += 1;
        }
        for (t, &len) in table_len.iter().enumerate().skip(1) {
            let table = &mut self.key_tables[t];
            if !table.live {
                continue;
            }
            if len == 0 {
                *table = KeyTable::dead();
                self.table_free.push(t as u32);
            } else {
                table.truncate(len as usize);
            }
        }
        if freed != 0 {
            self.shape_epoch += 1;
        }
        freed
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
        self.charge(STRING_BYTES + data.byte_len());
        self.strings_allocated += 1;
        let s = self.next_serial();
        StrId(self.strings.alloc(data, s))
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
            Units::Two(u) => self.alloc_str_u16(u),
        }
    }

    /// A string from borrowed UTF-16 code units: one copy, stored one-byte
    /// when possible.
    pub fn alloc_str_u16(&mut self, units: &[u16]) -> StrId {
        let repr = if units.iter().all(|&u| u <= 0xFF) {
            Repr::One(units.iter().map(|&u| u as u8).collect())
        } else {
            Repr::Two(units.into())
        };
        self.alloc_string(StringData::new(repr))
    }

    /// The string a builder holds, without a conversion copy for one-byte
    /// content.
    pub fn alloc_str_builder(&mut self, b: crate::string::StrBuilder) -> StrId {
        match b {
            crate::string::StrBuilder::One(bytes) => self.alloc_str_latin1(bytes),
            crate::string::StrBuilder::Two(units) => self.alloc_str_utf16(units),
        }
    }

    /// The code units `start..end` of `id` as a string (the string itself
    /// when that is all of it).
    pub fn substring(&mut self, id: StrId, start: usize, end: usize) -> StrId {
        self.flatten(id);
        let repr = match &self.strings.get(id.0).repr {
            _ if start == 0 && end == self.strings.get(id.0).len() => return id,
            Repr::One(b) => Repr::One(b[start..end].into()),
            Repr::Two(u) => {
                let u = &u[start..end];
                if u.iter().all(|&c| c <= 0xFF) {
                    Repr::One(u.iter().map(|&c| c as u8).collect())
                } else {
                    Repr::Two(u.into())
                }
            }
            Repr::Rope { .. } => unreachable!("flattened above"),
        };
        self.alloc_string(StringData::new(repr))
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
        // The flat buffer is new storage; the rope node counted 16 bytes.
        self.note_growth(if one_byte { len } else { len * 2 });
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

    /// The atom with this content, allocating only when there is none yet.
    pub fn atom_units(&mut self, units: Units<'_>) -> StrId {
        let hash = units.hash();
        let strings = &self.strings;
        if let Some(&found) = self.intern.find(hash, |&cand| {
            let c = strings.get(cand.0);
            c.len() == units.len() && c.units().eq(&units)
        }) {
            return found;
        }
        let id = self.alloc_str_units(units);
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
