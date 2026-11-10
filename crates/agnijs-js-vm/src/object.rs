//! The object model: shapes (hidden classes) with a dictionary fallback,
//! dense or sparse element storage, and the internal-slot payload of each
//! object kind.
//!
//! This module holds the *storage* operations: finding, adding, replacing
//! and removing own properties without running any JavaScript. The spec's
//! internal methods ([[Get]], [[Set]], [[DefineOwnProperty]] and the
//! exotic variants), which may call getters, setters and proxy traps, live
//! in `crate::ops`.

use crate::heap::{Heap, DICT_ENTRY_BYTES, ELEMENT_BYTES, SLOT_BYTES};
use crate::interp::generator::GeneratorData;
use crate::symbol::SymbolKind;
use crate::value::{
    Attrs, BigId, EnvId, ObjId, OptObj, PropKey, PropSlot, Property, ShapeId, StrId, SymId, Value,
};
use crate::vm::{JsResult, Vm};
use agnijs_engine_api::{HostClassId, HostFunction, HostObject, RealmId};
use agnijs_js_compiler::bytecode::CodeBlock;
use hashbrown::HashMap;
use indexmap::IndexMap;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;

/// Past this many named properties an object switches to dictionary mode.
pub(crate) const DICT_THRESHOLD: usize = 64;
/// Past this gap beyond the dense length, element writes go sparse.
const DENSE_GAP: u32 = 4096;

// ---- shapes -----------------------------------------------------------------

/// A shape: the ordered named properties (keys and attributes) of every
/// object sharing it. Slot `i` of an object holds the value of key `i`.
///
/// The keys themselves live in a [`KeyTable`] that every shape of one
/// transition chain shares: a shape owns the first `len` entries of its
/// table. Adding a property to the shape at the table's tip appends one
/// entry (amortised O(1)); a second transition off the same shape forks
/// the table, copying only that prefix. Readers go through
/// `Heap::shape_lookup`, `Heap::shape_key` and `Heap::shape_keys`.
pub struct Shape {
    pub table: KeyTableId,
    pub len: u32,
    /// The first transition off this shape, checked without hashing
    /// (most shapes have at most one), then any others.
    first: Option<((PropKey, Attrs), ShapeId)>,
    transitions: HashMap<(PropKey, Attrs), ShapeId>,
    pub parent: Option<ShapeId>,
    /// False for a freed id awaiting reuse (`Heap::sweep_shapes`).
    pub(crate) live: bool,
}

impl Shape {
    /// The empty shape, over the heap's first (empty) key table.
    pub fn root() -> Shape {
        Shape {
            table: KeyTableId(0),
            len: 0,
            first: None,
            transitions: HashMap::new(),
            parent: None,
            live: true,
        }
    }

    /// The placeholder left at a freed id.
    pub(crate) fn dead() -> Shape {
        Shape {
            live: false,
            ..Shape::root()
        }
    }

    /// Forget the transition to `child` (which is being freed), whose
    /// last key is `key`.
    pub(crate) fn unlink(&mut self, key: (PropKey, Attrs), child: ShapeId) {
        if self.first == Some((key, child)) {
            self.first = None;
        } else if self.transitions.get(&key) == Some(&child) {
            self.transitions.remove(&key);
        }
    }

    /// The shape reached by adding `key`, if that transition exists.
    #[inline]
    fn transition(&self, key: (PropKey, Attrs)) -> Option<ShapeId> {
        match self.first {
            Some((k, s)) if k == key => Some(s),
            _ if self.transitions.is_empty() => None,
            _ => self.transitions.get(&key).copied(),
        }
    }

    fn add_transition(&mut self, key: (PropKey, Attrs), child: ShapeId) {
        if self.first.is_none() {
            self.first = Some((key, child));
        } else {
            self.transitions.insert(key, child);
        }
    }

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Index of a [`KeyTable`] in `Heap::key_tables`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyTableId(pub u32);

/// The ordered keys of one transition chain, appended to as the chain
/// grows. Up to [`KeyTable::INDEX_FROM`] keys a lookup scans the prefix;
/// past that the table carries a hash index of every key it holds, which
/// a shape consults for positions below its own `len`.
pub struct KeyTable {
    pub keys: Vec<(PropKey, Attrs)>,
    index: HashMap<PropKey, u32>,
    /// False for a freed id awaiting reuse (`Heap::sweep_shapes`).
    pub(crate) live: bool,
}

impl Default for KeyTable {
    fn default() -> Self {
        KeyTable {
            keys: Vec::new(),
            index: HashMap::new(),
            live: true,
        }
    }
}

impl KeyTable {
    /// Lookups over this many keys or fewer scan; beyond it they hash.
    const INDEX_FROM: usize = 8;

    fn with_keys(keys: Vec<(PropKey, Attrs)>) -> KeyTable {
        let mut t = KeyTable::default();
        for (k, a) in keys {
            t.push(k, a);
        }
        t
    }

    fn push(&mut self, key: PropKey, attrs: Attrs) {
        let i = self.keys.len() as u32;
        self.keys.push((key, attrs));
        if self.keys.len() == KeyTable::INDEX_FROM + 1 {
            self.index.reserve(self.keys.len());
            for (j, (k, _)) in self.keys.iter().enumerate() {
                self.index.insert(*k, j as u32);
            }
        } else if self.keys.len() > KeyTable::INDEX_FROM {
            self.index.insert(key, i);
        }
    }

    /// The placeholder left at a freed id.
    pub(crate) fn dead() -> KeyTable {
        KeyTable {
            live: false,
            ..KeyTable::default()
        }
    }

    /// Drop the keys past `len` (no surviving shape owns them), so the
    /// longest surviving shape is the table's tip again and extends it in
    /// place.
    pub(crate) fn truncate(&mut self, len: usize) {
        if self.keys.len() <= len {
            return;
        }
        self.keys.truncate(len);
        if len <= KeyTable::INDEX_FROM {
            self.index = HashMap::new();
        } else {
            self.index.retain(|_, i| (*i as usize) < len);
        }
    }

    #[inline]
    fn lookup(&self, len: usize, key: PropKey) -> Option<u32> {
        if len <= KeyTable::INDEX_FROM {
            self.keys[..len].iter().position(|(k, _)| *k == key).map(|i| i as u32)
        } else {
            self.index.get(&key).copied().filter(|&i| (i as usize) < len)
        }
    }
}

// ---- elements ---------------------------------------------------------------

/// Integer-indexed properties.
pub enum Elements {
    /// `Empty` marks a hole. Every present element is a writable,
    /// enumerable, configurable data property.
    Dense(Vec<Value>),
    /// Anything else: sparse indices, non-default attributes, accessors.
    /// Boxed: rare, and it keeps `Elements` at the size of a `Vec`.
    Sparse(Box<BTreeMap<u32, Property>>),
}

impl Elements {
    pub fn len_hint(&self) -> usize {
        match self {
            Elements::Dense(v) => v.len(),
            Elements::Sparse(m) => m.len(),
        }
    }
}

// ---- object kinds -----------------------------------------------------------

/// The Rust signature of a built-in function.
pub type NativeFn = fn(&mut Vm, &CallArgs) -> JsResult<Value>;

/// Arguments of a native call.
pub struct CallArgs<'a> {
    pub this: Value,
    pub args: &'a [Value],
    /// `Undefined` for a plain call.
    pub new_target: Value,
    pub callee: ObjId,
}

impl<'a> CallArgs<'a> {
    #[inline]
    pub fn arg(&self, i: usize) -> Value {
        self.args.get(i).copied().unwrap_or(Value::Undefined)
    }
    #[inline]
    pub fn len(&self) -> usize {
        self.args.len()
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.args.is_empty()
    }
    #[inline]
    pub fn is_construct(&self) -> bool {
        !self.new_target.is_undefined()
    }
}

pub enum NativeKind {
    Fn(NativeFn),
    Host(Rc<HostFunction>),
}

/// Natives the interpreter recognises at a call site and replaces by a
/// frame push in the current run loop (TASK-100.19). The native itself
/// stays the generic path, for every case the interpreter does not inline.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum NativeTag {
    #[default]
    None,
    /// `Function.prototype.call`.
    FunctionCall,
    /// `Function.prototype.apply`.
    FunctionApply,
    /// `RegExp.prototype.exec`: the RegExp built-ins that call exec only
    /// to read the match skip building the result array when exec is this
    /// one (builtins::regexp).
    RegExpExec,
}

pub struct NativeData {
    pub func: NativeKind,
    pub tag: NativeTag,
    pub realm: RealmId,
    /// Has [[Construct]].
    pub constructor: bool,
    /// Captured values (traced): promise capabilities, revocable proxies...
    pub slots: Vec<Value>,
    /// A shared flag for paired closures (promise resolving functions).
    pub flag: Option<Rc<Cell<bool>>>,
    /// A shared counter for element closures (Promise.all remaining count).
    pub counter: Option<Rc<Cell<usize>>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConstructorKind {
    /// Not a constructor (methods, arrows, generators, async functions).
    None,
    Base,
    Derived,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThisMode {
    /// Arrow functions: `this` comes from the environment.
    Lexical,
    Strict,
    /// Sloppy: undefined/null `this` becomes the global object, primitives
    /// are boxed.
    Global,
}

#[derive(Clone, Debug)]
pub enum FieldKey {
    Prop(PropKey),
    Private(SymId),
}

#[derive(Clone, Debug)]
pub struct FieldDef {
    pub key: FieldKey,
    /// The initialiser function (a `ClassInitializer` closure) or none.
    pub init: Option<ObjId>,
    /// An anonymous function value takes the key as its name.
    pub anon: bool,
}

#[derive(Default)]
pub struct ClassData {
    pub fields: Vec<FieldDef>,
    /// Private methods and accessors to brand each instance with.
    pub private_methods: Vec<SymId>,
}

pub struct FunctionData {
    pub code: Rc<CodeBlock>,
    pub env: Option<EnvId>,
    pub realm: RealmId,
    pub home_object: Option<ObjId>,
    pub constructor_kind: ConstructorKind,
    pub this_mode: ThisMode,
    pub is_class_constructor: bool,
    pub class_data: Option<Box<ClassData>>,
    /// The module this function was created in (for `import.meta` and
    /// dynamic import referrers).
    pub module: Option<ObjId>,
}

pub struct BoundData {
    pub target: ObjId,
    pub this: Value,
    pub args: Vec<Value>,
}

pub struct MappedArgs {
    pub env: EnvId,
    /// `map[i]` is the environment slot that argument `i` aliases, until
    /// the mapping is removed.
    pub map: Vec<Option<u16>>,
}

pub struct ForInState {
    pub object: Option<ObjId>,
    pub keys: Vec<PropKey>,
    pub index: usize,
    pub visited: Vec<PropKey>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IterKind {
    Keys,
    Values,
    Entries,
}

pub struct ArrayIterState {
    pub target: Option<ObjId>,
    pub index: u32,
    pub kind: IterKind,
}

pub struct HostObjectData {
    pub class: HostClassId,
    pub data: Box<dyn HostObject>,
}

/// The class tags compiled code tests ([`Class`] pins these variants to
/// them; they are the declaration-order numbers the others follow).
pub mod class_tag {
    pub const ARRAY: u8 = 1;
    pub const STRING: u8 = 8;
    pub const ARGUMENTS: u8 = 11;
    pub const TYPED_ARRAY: u8 = 28;
    pub const PROXY: u8 = 30;
    pub const NAMESPACE: u8 = 31;
}

/// Internal slots: what makes an object more than a property bag.
/// `#[repr(u8)]`: the first byte is the tag, numbered in declaration order
/// (ADR 0019 J5), with the variants compiled code tests pinned to
/// [`class_tag`]. Variants wider than 16 bytes are boxed, so a `Class` is
/// 24 bytes.
#[repr(u8)]
pub enum Class {
    Ordinary,
    Array(ArrayData) = class_tag::ARRAY,
    Function(Box<FunctionData>),
    Native(Box<NativeData>),
    Bound(Box<BoundData>),
    Error,
    Boolean(bool),
    Number(f64),
    String(StrId) = class_tag::STRING,
    Symbol(SymId),
    BigInt(BigId),
    Arguments(Option<Box<MappedArgs>>) = class_tag::ARGUMENTS,
    ForIn(Box<ForInState>),
    ArrayIterator(ArrayIterState),
    StringIterator { string: Option<StrId>, index: u32 },
    Generator(Box<GeneratorData>),
    Promise(Box<crate::builtins::promise::PromiseData>),
    Date(f64),
    RegExp(Box<crate::builtins::regexp::RegExpData>),
    Map(Box<crate::builtins::collections::MapData>),
    Set(Box<crate::builtins::collections::MapData>),
    WeakMap(Box<crate::builtins::collections::MapData>),
    WeakSet(Box<crate::builtins::collections::MapData>),
    MapIterator(Box<crate::builtins::collections::MapIterState>),
    SetIterator(Box<crate::builtins::collections::MapIterState>),
    WeakRef(Value),
    FinalizationRegistry(Box<crate::builtins::collections::FinRegData>),
    ArrayBuffer(Box<crate::builtins::typedarray::BufferData>),
    TypedArray(Box<crate::builtins::typedarray::TypedArrayData>) = class_tag::TYPED_ARRAY,
    DataView(Box<crate::builtins::typedarray::DataViewData>),
    Proxy(Box<crate::builtins::proxy::ProxyData>) = class_tag::PROXY,
    Namespace(Box<crate::modules::NamespaceData>) = class_tag::NAMESPACE,
    Module(Box<crate::modules::ModuleRecord>),
    AsyncFromSyncIterator(Box<IteratorRecord>),
    RegExpStringIterator(Box<crate::builtins::regexp::RegExpIterState>),
    IteratorHelper(Box<crate::builtins::iterator::HelperState>),
    WrapForValidIterator(Box<IteratorRecord>),
    Host(Box<HostObjectData>),
    /// An ECMA-402 service object (`Intl.NumberFormat` and friends).
    Intl(Box<crate::builtins::intl::IntlData>),
}

/// An iterator and its `next` method.
#[derive(Clone, Copy, Debug)]
pub struct IteratorRecord {
    pub iterator: Value,
    pub next: Value,
}

#[derive(Clone, Copy, Debug)]
pub struct ArrayData {
    pub length: u32,
    pub length_writable: bool,
}

impl Class {
    /// Whether the payload lives in its own heap block.
    pub fn is_boxed(&self) -> bool {
        !matches!(
            self,
            Class::Ordinary
                | Class::Array(_)
                | Class::Error
                | Class::Boolean(_)
                | Class::Number(_)
                | Class::String(_)
                | Class::Symbol(_)
                | Class::BigInt(_)
                | Class::Arguments(None)
                | Class::ArrayIterator(_)
                | Class::StringIterator { .. }
                | Class::Date(_)
                | Class::WeakRef(_)
        )
    }

    pub fn is_callable(&self) -> bool {
        match self {
            Class::Function(_) | Class::Native(_) | Class::Bound(_) => true,
            Class::Proxy(p) => p.callable,
            _ => false,
        }
    }
}

// ---- object data ------------------------------------------------------------

/// Property slots held inside the object itself; slot `i` is inline when
/// `i < INLINE_SLOTS` and in the overflow buffer at `i - INLINE_SLOTS`
/// otherwise (ADR 0019 J5).
pub const INLINE_SLOTS: usize = 4;

/// An object. The header (shape, slot count, flags, prototype, class) and
/// the inline slots have a defined layout (`#[repr(C)]`, offsets below) for
/// the inline-cache hit path a later tier compiles (ADR 0019 J5):
///
/// - `shape` at [`ObjectData::OFFSET_SHAPE`], a `u32` shape id;
/// - `dict` at [`ObjectData::OFFSET_DICT`], non-null in dictionary mode;
/// - `class` at [`ObjectData::OFFSET_CLASS`], whose first byte is the
///   class tag (`Class` is `#[repr(u8)]`: tags in declaration order);
/// - the inline slots at [`ObjectData::OFFSET_INLINE`], [`INLINE_SLOTS`]
///   contiguous 16-byte [`PropSlot`]s.
///
/// Slots past the object's slot count hold `undefined` data, so the
/// tracer and a reader of a stale slot index never see a dead value.
#[repr(C)]
pub struct ObjectData {
    pub shape: ShapeId,
    /// Named slots in use (the shape's length; 0 in dictionary mode).
    nslots: u16,
    pub extensible: bool,
    /// An inline-cache entry looked through or into this object on a
    /// prototype chain: changes to its keys, attributes or prototype, and
    /// its death, move `Heap::proto_epoch` (TASK-100.20). Never cleared.
    pub watched: bool,
    /// The prototype, `u32::MAX` for none: read with [`ObjectData::proto`],
    /// changed through `Heap::set_proto`, which keeps the prototype epoch.
    proto: OptObj,
    /// Dictionary mode: replaces `shape` and the slots.
    pub dict: Option<Box<IndexMap<PropKey, Property>>>,
    pub class: Class,
    /// Values of the shape's first keys, by slot (16-byte aligned within
    /// the object, so no slot straddles a cache line).
    inline: [PropSlot; INLINE_SLOTS],
    pub elements: Elements,
    /// Values of the shape's keys from [`INLINE_SLOTS`] on.
    overflow: Vec<PropSlot>,
}

impl ObjectData {
    pub const OFFSET_SHAPE: usize = std::mem::offset_of!(ObjectData, shape);
    pub const OFFSET_DICT: usize = std::mem::offset_of!(ObjectData, dict);
    pub const OFFSET_CLASS: usize = std::mem::offset_of!(ObjectData, class);
    pub const OFFSET_INLINE: usize = std::mem::offset_of!(ObjectData, inline);
    pub const OFFSET_PROTO: usize = std::mem::offset_of!(ObjectData, proto);
    pub const OFFSET_ELEMENTS: usize = std::mem::offset_of!(ObjectData, elements);
    pub const OFFSET_OVERFLOW: usize = std::mem::offset_of!(ObjectData, overflow);

    /// The prototype.
    #[inline]
    pub fn proto(&self) -> Option<ObjId> {
        self.proto.get()
    }

    /// The prototype's id, `u32::MAX` for none.
    #[inline]
    pub fn proto_raw(&self) -> u32 {
        self.proto.raw()
    }

    /// Set the prototype without the epoch bookkeeping of `Heap::set_proto`.
    #[inline]
    pub(crate) fn set_proto_raw(&mut self, proto: Option<ObjId>) {
        self.proto = OptObj::from_option(proto);
    }

    pub fn new(proto: Option<ObjId>, class: Class) -> ObjectData {
        ObjectData {
            shape: ShapeId(0),
            nslots: 0,
            extensible: true,
            watched: false,
            proto: OptObj::from_option(proto),
            class,
            inline: [PropSlot::UNDEFINED; INLINE_SLOTS],
            overflow: Vec::new(),
            dict: None,
            elements: Elements::Dense(Vec::new()),
        }
    }

    /// Number of shape-described slots in use.
    #[inline]
    pub fn slot_count(&self) -> usize {
        self.nslots as usize
    }

    /// Slot `i`, which must be below the slot count (an inline index
    /// past it reads undefined).
    #[inline]
    pub fn slot(&self, i: u32) -> Option<&PropSlot> {
        // An unused inline slot holds undefined data, so only the
        // overflow needs a bound.
        let i = i as usize;
        debug_assert!(i < self.nslots as usize);
        if i < INLINE_SLOTS {
            Some(&self.inline[i])
        } else {
            self.overflow.get(i - INLINE_SLOTS)
        }
    }

    #[inline]
    pub fn slot_mut(&mut self, i: u32) -> Option<&mut PropSlot> {
        let i = i as usize;
        debug_assert!(i < self.nslots as usize);
        if i < INLINE_SLOTS {
            Some(&mut self.inline[i])
        } else {
            self.overflow.get_mut(i - INLINE_SLOTS)
        }
    }

    /// Slot `i`, which the object's shape says it has.
    #[inline]
    pub fn slot_at(&self, i: u32) -> PropSlot {
        *self.slot(i).expect("slot index beyond the object's slots")
    }

    /// Every slot in use, in slot order.
    #[inline]
    pub fn slots(&self) -> impl Iterator<Item = &PropSlot> {
        let n = (self.nslots as usize).min(INLINE_SLOTS);
        self.inline[..n].iter().chain(self.overflow.iter())
    }

    /// Append a slot (the shape grew by one key). The overflow buffer, when
    /// it is first needed, comes from `pool`. Returns the bytes of new
    /// storage (none for an inline slot).
    #[inline]
    pub(crate) fn push_slot(&mut self, s: PropSlot, pool: &mut crate::heap::BufPool<PropSlot>) -> usize {
        let i = self.nslots as usize;
        self.nslots += 1;
        if i < INLINE_SLOTS {
            self.inline[i] = s;
            0
        } else {
            if self.overflow.capacity() == 0 {
                self.overflow = pool.take();
            }
            debug_assert_eq!(self.overflow.len(), i - INLINE_SLOTS);
            self.overflow.push(s);
            SLOT_BYTES
        }
    }

    /// Drop the last slot (the shape went back to its parent).
    #[inline]
    pub(crate) fn pop_slot(&mut self) {
        let n = self.nslots as usize;
        debug_assert!(n > 0);
        self.nslots -= 1;
        if n <= INLINE_SLOTS {
            self.inline[n - 1] = PropSlot::UNDEFINED;
        } else {
            self.overflow.pop();
        }
    }

    /// Empty the slots (dictionary mode, or a dead object); returns the
    /// overflow buffer.
    #[inline]
    pub(crate) fn take_slots(&mut self) -> Vec<PropSlot> {
        let n = (self.nslots as usize).min(INLINE_SLOTS);
        for s in &mut self.inline[..n] {
            *s = PropSlot::UNDEFINED;
        }
        self.nslots = 0;
        std::mem::take(&mut self.overflow)
    }

    /// Approximate bytes of the storage that grows after allocation, at the
    /// sizes the heap counts growth with (inline slots are part of the
    /// object).
    pub fn extra_bytes(&self) -> usize {
        self.overflow.len() * SLOT_BYTES
            + self
                .dict
                .as_ref()
                .map(|d| d.len() * DICT_ENTRY_BYTES)
                .unwrap_or(0)
            + match &self.elements {
                Elements::Dense(v) => v.len() * ELEMENT_BYTES,
                Elements::Sparse(m) => m.len() * DICT_ENTRY_BYTES,
            }
    }

    pub fn is_callable(&self) -> bool {
        self.class.is_callable()
    }

    /// Rust heap blocks this object owns directly: its class payload, its
    /// overflow slots, its dictionary and its elements (diagnostics and
    /// tests; the object itself lives in the arena).
    pub fn owned_blocks(&self) -> usize {
        let elements = match &self.elements {
            Elements::Dense(v) => v.capacity() != 0,
            Elements::Sparse(_) => true,
        };
        self.class.is_boxed() as usize
            + (self.overflow.capacity() != 0) as usize
            + self.dict.is_some() as usize
            + elements as usize
    }

    pub fn is_array(&self) -> bool {
        matches!(self.class, Class::Array(_))
    }

    pub fn array_length(&self) -> Option<u32> {
        match &self.class {
            Class::Array(a) => Some(a.length),
            _ => None,
        }
    }

    pub fn function(&self) -> Option<&FunctionData> {
        match &self.class {
            Class::Function(f) => Some(f),
            _ => None,
        }
    }

    pub fn function_mut(&mut self) -> Option<&mut FunctionData> {
        match &mut self.class {
            Class::Function(f) => Some(f),
            _ => None,
        }
    }

    pub fn native(&self) -> Option<&NativeData> {
        match &self.class {
            Class::Native(n) => Some(n),
            _ => None,
        }
    }

    pub fn native_mut(&mut self) -> Option<&mut NativeData> {
        match &mut self.class {
            Class::Native(n) => Some(n),
            _ => None,
        }
    }

    pub fn is_proxy(&self) -> bool {
        matches!(self.class, Class::Proxy(_))
    }

    /// Number of named (non-index) own properties.
    pub fn named_count(&self) -> usize {
        match &self.dict {
            Some(d) => d.len(),
            None => self.nslots as usize,
        }
    }
}

// ---- storage operations -----------------------------------------------------

impl Heap {
    /// The slot of `key` in `shape`, if the shape has it.
    #[inline]
    pub fn shape_lookup(&self, shape: ShapeId, key: PropKey) -> Option<u32> {
        let sh = self.shape(shape);
        self.key_tables[sh.table.0 as usize].lookup(sh.len as usize, key)
    }

    /// The key and attributes stored in `slot` of `shape`.
    #[inline]
    pub fn shape_key(&self, shape: ShapeId, slot: u32) -> (PropKey, Attrs) {
        let sh = self.shape(shape);
        debug_assert!(slot < sh.len);
        self.key_tables[sh.table.0 as usize].keys[slot as usize]
    }

    /// Every key of `shape`, in slot order.
    #[inline]
    pub fn shape_keys(&self, shape: ShapeId) -> &[(PropKey, Attrs)] {
        let sh = self.shape(shape);
        &self.key_tables[sh.table.0 as usize].keys[..sh.len as usize]
    }

    /// Transition `shape` by adding `key` with `attrs`.
    pub(crate) fn shape_add(&mut self, shape: ShapeId, key: PropKey, attrs: Attrs) -> ShapeId {
        if let Some(next) = self.shape(shape).transition((key, attrs)) {
            return next;
        }
        let (mut table, len) = {
            let p = self.shape(shape);
            (p.table, p.len as usize)
        };
        if self.key_tables[table.0 as usize].keys.len() != len {
            // Another transition already extended this table past
            // `shape`: the new chain gets its own copy of the prefix.
            let prefix = self.key_tables[table.0 as usize].keys[..len].to_vec();
            table = self.alloc_key_table(KeyTable::with_keys(prefix));
        }
        self.key_tables[table.0 as usize].push(key, attrs);
        let new = Shape {
            table,
            len: len as u32 + 1,
            first: None,
            transitions: HashMap::new(),
            parent: Some(shape),
            live: true,
        };
        let id = self.alloc_shape(new);
        self.shape_mut(shape).add_transition((key, attrs), id);
        id
    }

    /// The shape reached from the empty shape by adding `keys` in order.
    pub(crate) fn shape_with(&mut self, keys: &[(PropKey, Attrs)]) -> ShapeId {
        keys.iter()
            .fold(ShapeId(0), |s, &(k, a)| self.shape_add(s, k, a))
    }

    /// Give a new object, which has no named properties yet, the shape
    /// `shape` and one slot per key of it.
    pub(crate) fn init_named(&mut self, obj: ObjId, shape: ShapeId, slots: &[PropSlot]) {
        debug_assert_eq!(self.shape(shape).len as usize, slots.len());
        let o = self.objects.get_mut(obj.0);
        debug_assert!(o.slot_count() == 0 && o.dict.is_none() && o.shape == ShapeId(0));
        o.shape = shape;
        let mut grown = 0;
        for &s in slots {
            grown += o.push_slot(s, &mut self.slot_pool);
        }
        self.note_growth(grown);
    }

    /// Add a data property `key` with every attribute set to an ordinary
    /// object when its shape already has that transition, which also means
    /// the key is absent; `false` (nothing done) otherwise. The common case
    /// of an object literal's named properties.
    #[inline]
    pub(crate) fn add_by_transition(&mut self, obj: ObjId, key: PropKey, v: Value) -> bool {
        let o = self.obj(obj);
        if o.dict.is_some()
            || !o.extensible
            || o.watched
            || !matches!(o.class, Class::Ordinary)
            || matches!(key, PropKey::Index(_))
        {
            return false;
        }
        let Some(next) = self.shape(o.shape).transition((key, Attrs::ALL)) else {
            return false;
        };
        self.add_with_shape(obj, next, PropSlot::Data(v));
        true
    }

    /// Move `obj` (not a dictionary) to `next`, a transition of its shape,
    /// storing the new property's `slot`.
    #[inline]
    pub(crate) fn add_with_shape(&mut self, obj: ObjId, next: ShapeId, slot: PropSlot) {
        debug_assert_eq!(self.shape(next).parent, Some(self.obj(obj).shape));
        let o = self.objects.get_mut(obj.0);
        o.shape = next;
        let grown = o.push_slot(slot, &mut self.slot_pool);
        self.note_growth(grown);
    }

    /// Switch an object to dictionary mode.
    pub fn make_dictionary(&mut self, obj: ObjId) {
        let o = self.obj(obj);
        if o.dict.is_some() {
            return;
        }
        if o.watched {
            self.bump_proto_epoch();
        }
        let o = self.obj(obj);
        let mut dict = IndexMap::with_capacity(o.slot_count() + 8);
        for ((k, attrs), slot) in self.shape_keys(o.shape).iter().zip(o.slots()) {
            dict.insert(
                *k,
                Property {
                    slot: *slot,
                    attrs: *attrs,
                },
            );
        }
        let grown = dict.len() * DICT_ENTRY_BYTES;
        let o = self.objects.get_mut(obj.0);
        o.dict = Some(Box::new(dict));
        let overflow = o.take_slots();
        o.shape = ShapeId(0);
        self.slot_pool.give(overflow);
        self.note_growth(grown);
    }

    /// Switch an object's elements to sparse storage.
    pub fn make_sparse(&mut self, obj: ObjId) {
        let o = self.obj_mut(obj);
        if let Elements::Dense(v) = &mut o.elements {
            let mut map = BTreeMap::new();
            for (i, val) in v.iter().enumerate() {
                if !val.is_empty() {
                    map.insert(i as u32, Property::data(*val, Attrs::ALL));
                }
            }
            let grown = map.len() * DICT_ENTRY_BYTES;
            o.elements = Elements::Sparse(Box::new(map));
            self.note_growth(grown);
        }
    }

    /// Find an own property's storage (no exotic behaviour).
    pub fn get_own_property(&self, obj: ObjId, key: PropKey) -> Option<Property> {
        let o = self.obj(obj);
        match key {
            PropKey::Index(i) => match &o.elements {
                Elements::Dense(v) => {
                    let val = *v.get(i as usize)?;
                    if val.is_empty() {
                        None
                    } else {
                        Some(Property::data(val, Attrs::ALL))
                    }
                }
                Elements::Sparse(m) => m.get(&i).copied(),
            },
            _ => {
                if let Some(d) = &o.dict {
                    d.get(&key).copied()
                } else {
                    let slot = self.shape_lookup(o.shape, key)?;
                    Some(Property {
                        slot: o.slot_at(slot),
                        attrs: self.shape_key(o.shape, slot).1,
                    })
                }
            }
        }
    }

    pub fn has_own_property(&self, obj: ObjId, key: PropKey) -> bool {
        self.get_own_property(obj, key).is_some()
    }

    /// Add a property that is known to be absent. New storage (a dense
    /// element and any holes before it, a sparse entry, a slot, a
    /// dictionary entry) counts towards the next collection.
    pub fn add_own_property(&mut self, obj: ObjId, key: PropKey, prop: Property) {
        match key {
            PropKey::Index(i) => {
                let is_default_data = !prop.is_accessor() && prop.attrs == Attrs::ALL;
                let o = self.objects.get_mut(obj.0);
                let grown = match &mut o.elements {
                    Elements::Dense(v)
                        if is_default_data && i as usize <= v.len() + DENSE_GAP as usize =>
                    {
                        let idx = i as usize;
                        if idx < v.len() {
                            v[idx] = prop.value().unwrap();
                            0
                        } else {
                            let added = idx + 1 - v.len();
                            if v.capacity() == 0 {
                                *v = self.value_pool.take();
                            }
                            if idx > v.len() {
                                v.resize(idx, Value::Empty);
                            }
                            v.push(prop.value().unwrap());
                            added * ELEMENT_BYTES
                        }
                    }
                    _ => {
                        self.make_sparse(obj);
                        let o = self.obj_mut(obj);
                        if let Elements::Sparse(m) = &mut o.elements {
                            m.insert(i, prop);
                        }
                        DICT_ENTRY_BYTES
                    }
                };
                if let Class::Array(a) = &mut self.obj_mut(obj).class {
                    if i >= a.length {
                        a.length = i + 1;
                    }
                }
                self.note_growth(grown);
            }
            _ => {
                self.note_structure_change(obj);
                let o = self.obj(obj);
                if o.dict.is_none() && o.slot_count() >= DICT_THRESHOLD {
                    self.make_dictionary(obj);
                }
                let o = self.obj_mut(obj);
                if let Some(d) = &mut o.dict {
                    d.insert(key, prop);
                    self.note_growth(DICT_ENTRY_BYTES);
                } else {
                    let shape = o.shape;
                    let new_shape = self.shape_add(shape, key, prop.attrs);
                    let o = self.objects.get_mut(obj.0);
                    o.shape = new_shape;
                    let grown = o.push_slot(prop.slot, &mut self.slot_pool);
                    self.note_growth(grown);
                }
            }
        }
    }

    /// Replace an existing own property (value and/or attributes).
    pub fn replace_own_property(&mut self, obj: ObjId, key: PropKey, prop: Property) {
        match key {
            PropKey::Index(i) => {
                let is_default_data = !prop.is_accessor() && prop.attrs == Attrs::ALL;
                let o = self.obj_mut(obj);
                match &mut o.elements {
                    Elements::Dense(v) if is_default_data => {
                        v[i as usize] = prop.value().unwrap();
                    }
                    Elements::Dense(_) => {
                        self.make_sparse(obj);
                        if let Elements::Sparse(m) = &mut self.obj_mut(obj).elements {
                            m.insert(i, prop);
                        }
                    }
                    Elements::Sparse(m) => {
                        m.insert(i, prop);
                    }
                }
            }
            _ => {
                self.note_structure_change(obj);
                let o = self.obj_mut(obj);
                if let Some(d) = &mut o.dict {
                    d.insert(key, prop);
                    return;
                }
                let shape = o.shape;
                let slot = self
                    .shape_lookup(shape, key)
                    .expect("replace_own_property: missing key");
                let attrs = self.shape_key(shape, slot).1;
                if attrs == prop.attrs {
                    *self.obj_mut(obj).slot_mut(slot).expect("slot of the shape") = prop.slot;
                } else {
                    self.make_dictionary(obj);
                    self.obj_mut(obj).dict.as_mut().unwrap().insert(key, prop);
                }
            }
        }
    }

    /// Set the value of an existing own data property, keeping attributes.
    pub fn set_own_value(&mut self, obj: ObjId, key: PropKey, value: Value) {
        match key {
            PropKey::Index(i) => {
                let o = self.obj_mut(obj);
                match &mut o.elements {
                    Elements::Dense(v) => v[i as usize] = value,
                    Elements::Sparse(m) => {
                        if let Some(p) = m.get_mut(&i) {
                            p.slot = PropSlot::Data(value);
                        }
                    }
                }
            }
            _ => {
                let o = self.obj_mut(obj);
                if let Some(d) = &mut o.dict {
                    if let Some(p) = d.get_mut(&key) {
                        p.slot = PropSlot::Data(value);
                    }
                    return;
                }
                let shape = o.shape;
                if let Some(slot) = self.shape_lookup(shape, key) {
                    if let Some(s) = self.obj_mut(obj).slot_mut(slot) {
                        *s = PropSlot::Data(value);
                    }
                }
            }
        }
    }

    /// Remove an own property. Returns whether it existed.
    pub fn remove_own_property(&mut self, obj: ObjId, key: PropKey) -> bool {
        match key {
            PropKey::Index(i) => {
                let o = self.obj_mut(obj);
                match &mut o.elements {
                    Elements::Dense(v) => {
                        let idx = i as usize;
                        if idx < v.len() && !v[idx].is_empty() {
                            if idx + 1 == v.len() {
                                v.pop();
                                while v.last().is_some_and(|x| x.is_empty()) {
                                    v.pop();
                                }
                            } else {
                                v[idx] = Value::Empty;
                            }
                            true
                        } else {
                            false
                        }
                    }
                    Elements::Sparse(m) => m.remove(&i).is_some(),
                }
            }
            _ => {
                self.note_structure_change(obj);
                let o = self.obj_mut(obj);
                if let Some(d) = &mut o.dict {
                    return d.shift_remove(&key).is_some();
                }
                let shape = o.shape;
                let Some(slot) = self.shape_lookup(shape, key) else {
                    return false;
                };
                // Removing the most recently added key walks back to the
                // parent shape; anything else goes to dictionary mode.
                let sh = self.shape(shape);
                if slot + 1 == sh.len {
                    let parent = sh.parent.expect("non-root shape has a parent");
                    let o = self.obj_mut(obj);
                    o.pop_slot();
                    o.shape = parent;
                } else {
                    self.make_dictionary(obj);
                    self.obj_mut(obj).dict.as_mut().unwrap().shift_remove(&key);
                }
                true
            }
        }
    }

    /// Own keys in spec order: indices ascending, then strings and symbols
    /// in insertion order. Private names are omitted.
    pub fn own_keys(&self, obj: ObjId) -> Vec<PropKey> {
        let o = self.obj(obj);
        let mut keys = Vec::with_capacity(o.elements.len_hint() + o.named_count());
        match &o.elements {
            Elements::Dense(v) => {
                for (i, val) in v.iter().enumerate() {
                    if !val.is_empty() {
                        keys.push(PropKey::Index(i as u32));
                    }
                }
            }
            Elements::Sparse(m) => keys.extend(m.keys().map(|&i| PropKey::Index(i))),
        }
        let mut symbols = Vec::new();
        let push = |k: PropKey, keys: &mut Vec<PropKey>, symbols: &mut Vec<PropKey>| match k {
            PropKey::Sym(s) => {
                if self.sym(s).kind != SymbolKind::Private {
                    symbols.push(k);
                }
            }
            _ => keys.push(k),
        };
        if let Some(d) = &o.dict {
            for k in d.keys() {
                push(*k, &mut keys, &mut symbols);
            }
        } else {
            for (k, _) in self.shape_keys(o.shape) {
                push(*k, &mut keys, &mut symbols);
            }
        }
        keys.extend(symbols);
        keys
    }

    /// Own index keys only, ascending.
    pub fn own_index_keys(&self, obj: ObjId) -> Vec<u32> {
        match &self.obj(obj).elements {
            Elements::Dense(v) => v
                .iter()
                .enumerate()
                .filter(|(_, x)| !x.is_empty())
                .map(|(i, _)| i as u32)
                .collect(),
            Elements::Sparse(m) => m.keys().copied().collect(),
        }
    }

    /// Own string (non-index) keys in order.
    pub fn own_string_keys(&self, obj: ObjId) -> Vec<PropKey> {
        let o = self.obj(obj);
        let mut keys = Vec::new();
        if let Some(d) = &o.dict {
            keys.extend(d.keys().filter(|k| matches!(k, PropKey::Str(_))).copied());
        } else {
            keys.extend(
                self.shape_keys(o.shape)
                    .iter()
                    .map(|(k, _)| *k)
                    .filter(|k| matches!(k, PropKey::Str(_))),
            );
        }
        keys
    }

    /// Fast path for dense arrays: the element at `i` when present.
    #[inline]
    pub fn dense_element(&self, obj: ObjId, i: u32) -> Option<Value> {
        match &self.obj(obj).elements {
            Elements::Dense(v) => match v.get(i as usize) {
                Some(x) if !x.is_empty() => Some(*x),
                _ => None,
            },
            _ => None,
        }
    }

    /// Whether every element is in dense storage (no holes checked).
    pub fn is_dense(&self, obj: ObjId) -> bool {
        matches!(self.obj(obj).elements, Elements::Dense(_))
    }

    /// Dense elements without holes up to the array length.
    pub fn dense_values(&self, obj: ObjId) -> Option<&[Value]> {
        let o = self.obj(obj);
        match (&o.elements, &o.class) {
            (Elements::Dense(v), Class::Array(a))
                if v.len() == a.length as usize && !v.iter().any(|x| x.is_empty()) =>
            {
                Some(v)
            }
            _ => None,
        }
    }

    /// Append to a dense array (used by literals and `push`).
    pub fn array_push_dense(&mut self, obj: ObjId, value: Value) -> bool {
        let o = self.objects.get_mut(obj.0);
        if let (Elements::Dense(v), Class::Array(a)) = (&mut o.elements, &mut o.class) {
            if v.len() == a.length as usize && a.length < PropKey::MAX_INDEX {
                if v.capacity() == 0 {
                    *v = self.value_pool.take();
                }
                v.push(value);
                a.length += 1;
                self.note_growth(ELEMENT_BYTES);
                return true;
            }
        }
        false
    }

    /// Truncate elements at or beyond `new_len` (array length shrink).
    /// Returns the first index that could not be deleted, if any.
    pub fn truncate_elements(&mut self, obj: ObjId, new_len: u32) -> Option<u32> {
        let o = self.obj_mut(obj);
        match &mut o.elements {
            Elements::Dense(v) => {
                v.truncate(new_len as usize);
                while v.last().is_some_and(|x| x.is_empty()) {
                    v.pop();
                }
                None
            }
            Elements::Sparse(m) => {
                let to_delete: Vec<u32> = m.range(new_len..).map(|(k, _)| *k).collect();
                // Delete from the highest down; stop at the first
                // non-configurable element.
                for &i in to_delete.iter().rev() {
                    let p = m[&i];
                    if !p.attrs.configurable() {
                        return Some(i);
                    }
                    m.remove(&i);
                }
                None
            }
        }
    }
}

/// A plain object with the given prototype.
pub fn ordinary(proto: Option<ObjId>) -> ObjectData {
    ObjectData::new(proto, Class::Ordinary)
}

/// Where compiled code finds an object's parts (TASK-101.3.2), computed
/// once per process from these types and checked against live values;
/// `None`, and no inline property paths, if anything is not as compiled
/// code reads it. Everything here is safe code: offsets are differences
/// of addresses, never dereferenced.
#[cfg(feature = "jit")]
pub(crate) fn jit_heap_layout() -> Option<agnijs_js_jit::HeapLayout> {
    static LAYOUT: std::sync::OnceLock<Option<agnijs_js_jit::HeapLayout>> = std::sync::OnceLock::new();
    *LAYOUT.get_or_init(compute_jit_heap_layout)
}

#[cfg(feature = "jit")]
fn compute_jit_heap_layout() -> Option<agnijs_js_jit::HeapLayout> {
    use std::mem::size_of;
    fn addr<T>(r: &T) -> usize {
        r as *const T as usize
    }
    let word = size_of::<usize>();
    let u32_of = |n: usize| u32::try_from(n).ok();

    // Values: an object value's id is the `u32` at the payload offset.
    let v = Value::Object(ObjId(7));
    match &v {
        Value::Object(id) if addr(id) - addr(&v) == Value::PAYLOAD_OFFSET && size_of::<ObjId>() == 4 => {}
        _ => return None,
    }
    // A property slot is as large as a value with a data property's value
    // at its start: the accessor case's discriminant must then live in the
    // value's only niche, its tag byte, as a tag no value has.
    let s = PropSlot::UNDEFINED;
    match &s {
        PropSlot::Data(v) if addr(v) == addr(&s) && size_of::<PropSlot>() == size_of::<Value>() => {}
        _ => return None,
    }
    // The arena's slots, and the object inside one.
    let probe = Some(ordinary(None));
    let obj_offset = addr(probe.as_ref()?) - addr(&probe);
    if size_of::<ShapeId>() != 4 || size_of::<OptObj>() != 4 {
        return None;
    }
    // The dictionary and mapped-arguments words: `Option<Box<_>>`, null
    // exactly when `None`.
    if size_of::<Option<Box<IndexMap<PropKey, Property>>>>() != word
        || size_of::<Option<Box<MappedArgs>>>() != word
    {
        return None;
    }
    let c = Class::Arguments(None);
    let arguments_mapped = match &c {
        Class::Arguments(a) => addr(a) - addr(&c),
        _ => return None,
    };
    let c = Class::Array(ArrayData {
        length: 0,
        length_writable: true,
    });
    let array_length = match &c {
        Class::Array(a) => addr(&a.length) - addr(&c),
        _ => return None,
    };
    // Elements: the dense vector at the start of an `Elements` no larger
    // than it. The sparse case must then put its discriminant in one of
    // the vector's niches: a null buffer pointer or a capacity above
    // `isize::MAX`, which is what compiled code tests.
    let e = Elements::Dense(Vec::new());
    match &e {
        Elements::Dense(v) if addr(v) == addr(&e) && size_of::<Elements>() == size_of::<Vec<Value>>() => {}
        _ => return None,
    }
    let values = agnijs_js_jit::vec_layout(Value::Undefined)?;
    let slots = agnijs_js_jit::vec_layout(PropSlot::UNDEFINED)?;
    let el = ObjectData::OFFSET_ELEMENTS;
    let ov = ObjectData::OFFSET_OVERFLOW;
    let bit = |t: u8| 1u64 << t;
    Some(agnijs_js_jit::HeapLayout {
        obj_stride: u32_of(size_of::<Option<ObjectData>>())?,
        obj_offset: u32_of(obj_offset)?,
        shape: u32_of(ObjectData::OFFSET_SHAPE)?,
        dict: u32_of(ObjectData::OFFSET_DICT)?,
        class: u32_of(ObjectData::OFFSET_CLASS)?,
        proto: u32_of(ObjectData::OFFSET_PROTO)?,
        inline: u32_of(ObjectData::OFFSET_INLINE)?,
        inline_slots: u32_of(INLINE_SLOTS)?,
        overflow_ptr: u32_of(ov + slots.ptr as usize)?,
        overflow_len: u32_of(ov + slots.len as usize)?,
        array_length: u32_of(ObjectData::OFFSET_CLASS + array_length)?,
        elements_ptr: u32_of(el + values.ptr as usize)?,
        elements_cap: u32_of(el + values.cap as usize)?,
        elements_len: u32_of(el + values.len as usize)?,
        arguments_mapped: u32_of(ObjectData::OFFSET_CLASS + arguments_mapped)?,
        class_array: class_tag::ARRAY,
        class_string: class_tag::STRING,
        class_arguments: class_tag::ARGUMENTS,
        // Class tags are below 64: `Class` has fewer variants.
        not_receiver: bit(class_tag::TYPED_ARRAY) | bit(class_tag::NAMESPACE) | bit(class_tag::PROXY),
        not_elements: bit(class_tag::STRING)
            | bit(class_tag::TYPED_ARRAY)
            | bit(class_tag::NAMESPACE)
            | bit(class_tag::PROXY),
    })
}
