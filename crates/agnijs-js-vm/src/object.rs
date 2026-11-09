//! The object model: shapes (hidden classes) with a dictionary fallback,
//! dense or sparse element storage, and the internal-slot payload of each
//! object kind.
//!
//! This module holds the *storage* operations: finding, adding, replacing
//! and removing own properties without running any JavaScript. The spec's
//! internal methods ([[Get]], [[Set]], [[DefineOwnProperty]] and the
//! exotic variants), which may call getters, setters and proxy traps, live
//! in `crate::ops`.

use crate::heap::Heap;
use crate::interp::generator::GeneratorData;
use crate::symbol::SymbolKind;
use crate::value::{
    Attrs, BigId, EnvId, ObjId, PropKey, PropSlot, Property, ShapeId, StrId, SymId, Value,
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
const DICT_THRESHOLD: usize = 64;
/// Past this gap beyond the dense length, element writes go sparse.
const DENSE_GAP: u32 = 4096;

// ---- shapes -----------------------------------------------------------------

/// A shape: the ordered named properties (keys and attributes) of every
/// object sharing it. Slot `i` of an object holds the value of `keys[i]`.
pub struct Shape {
    pub keys: Vec<(PropKey, Attrs)>,
    index: HashMap<PropKey, u32>,
    transitions: HashMap<(PropKey, Attrs), ShapeId>,
    pub parent: Option<ShapeId>,
}

impl Shape {
    pub fn root() -> Shape {
        Shape {
            keys: Vec::new(),
            index: HashMap::new(),
            transitions: HashMap::new(),
            parent: None,
        }
    }

    #[inline]
    pub fn lookup(&self, key: PropKey) -> Option<u32> {
        if self.keys.len() <= 8 {
            self.keys
                .iter()
                .position(|(k, _)| *k == key)
                .map(|i| i as u32)
        } else {
            self.index.get(&key).copied()
        }
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

// ---- elements ---------------------------------------------------------------

/// Integer-indexed properties.
pub enum Elements {
    /// `Empty` marks a hole. Every present element is a writable,
    /// enumerable, configurable data property.
    Dense(Vec<Value>),
    /// Anything else: sparse indices, non-default attributes, accessors.
    Sparse(BTreeMap<u32, Property>),
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

pub struct NativeData {
    pub func: NativeKind,
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

/// Internal slots: what makes an object more than a property bag.
pub enum Class {
    Ordinary,
    Array(ArrayData),
    Function(Box<FunctionData>),
    Native(Box<NativeData>),
    Bound(Box<BoundData>),
    Error,
    Boolean(bool),
    Number(f64),
    String(StrId),
    Symbol(SymId),
    BigInt(BigId),
    Arguments(Option<Box<MappedArgs>>),
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
    TypedArray(Box<crate::builtins::typedarray::TypedArrayData>),
    DataView(Box<crate::builtins::typedarray::DataViewData>),
    Proxy(Box<crate::builtins::proxy::ProxyData>),
    Namespace(Box<crate::modules::NamespaceData>),
    Module(Box<crate::modules::ModuleRecord>),
    AsyncFromSyncIterator { iterator: Value, next: Value },
    RegExpStringIterator(Box<crate::builtins::regexp::RegExpIterState>),
    IteratorHelper(Box<crate::builtins::iterator::HelperState>),
    WrapForValidIterator { iterator: Value, next: Value },
    Host(Box<HostObjectData>),
    /// An ECMA-402 service object (`Intl.NumberFormat` and friends).
    Intl(Box<crate::builtins::intl::IntlData>),
}

#[derive(Clone, Copy, Debug)]
pub struct ArrayData {
    pub length: u32,
    pub length_writable: bool,
}

impl Class {
    pub fn is_callable(&self) -> bool {
        match self {
            Class::Function(_) | Class::Native(_) | Class::Bound(_) => true,
            Class::Proxy(p) => p.callable,
            _ => false,
        }
    }
}

// ---- object data ------------------------------------------------------------

pub struct ObjectData {
    pub proto: Option<ObjId>,
    pub shape: ShapeId,
    /// Values of the shape's keys, by slot.
    pub slots: Vec<PropSlot>,
    /// Dictionary mode: replaces `shape`/`slots`.
    pub dict: Option<Box<IndexMap<PropKey, Property>>>,
    pub elements: Elements,
    pub extensible: bool,
    pub class: Class,
}

impl ObjectData {
    pub fn new(proto: Option<ObjId>, class: Class) -> ObjectData {
        ObjectData {
            proto,
            shape: ShapeId(0),
            slots: Vec::new(),
            dict: None,
            elements: Elements::Dense(Vec::new()),
            extensible: true,
            class,
        }
    }

    pub fn extra_bytes(&self) -> usize {
        self.slots.len() * 24
            + self.dict.as_ref().map(|d| d.len() * 48).unwrap_or(0)
            + match &self.elements {
                Elements::Dense(v) => v.len() * 16,
                Elements::Sparse(m) => m.len() * 48,
            }
    }

    pub fn is_callable(&self) -> bool {
        self.class.is_callable()
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
            None => self.slots.len(),
        }
    }
}

// ---- storage operations -----------------------------------------------------

impl Heap {
    /// Transition `shape` by adding `key` with `attrs`.
    fn shape_add(&mut self, shape: ShapeId, key: PropKey, attrs: Attrs) -> ShapeId {
        if let Some(&next) = self.shape(shape).transitions.get(&(key, attrs)) {
            return next;
        }
        let parent = self.shape(shape);
        let mut keys = parent.keys.clone();
        keys.push((key, attrs));
        let mut index = HashMap::with_capacity(keys.len());
        for (i, (k, _)) in keys.iter().enumerate() {
            index.insert(*k, i as u32);
        }
        let new = Shape {
            keys,
            index,
            transitions: HashMap::new(),
            parent: Some(shape),
        };
        let id = self.alloc_shape(new);
        self.shape_mut(shape).transitions.insert((key, attrs), id);
        id
    }

    /// Switch an object to dictionary mode.
    pub fn make_dictionary(&mut self, obj: ObjId) {
        let o = self.obj(obj);
        if o.dict.is_some() {
            return;
        }
        let shape = self.shape(o.shape);
        let mut dict = IndexMap::with_capacity(o.slots.len() + 8);
        for (i, (k, attrs)) in shape.keys.iter().enumerate() {
            dict.insert(
                *k,
                Property {
                    slot: o.slots[i],
                    attrs: *attrs,
                },
            );
        }
        let o = self.obj_mut(obj);
        o.dict = Some(Box::new(dict));
        o.slots = Vec::new();
        o.shape = ShapeId(0);
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
            o.elements = Elements::Sparse(map);
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
                    let shape = self.shape(o.shape);
                    let slot = shape.lookup(key)?;
                    Some(Property {
                        slot: o.slots[slot as usize],
                        attrs: shape.keys[slot as usize].1,
                    })
                }
            }
        }
    }

    pub fn has_own_property(&self, obj: ObjId, key: PropKey) -> bool {
        self.get_own_property(obj, key).is_some()
    }

    /// Add a property that is known to be absent.
    pub fn add_own_property(&mut self, obj: ObjId, key: PropKey, prop: Property) {
        match key {
            PropKey::Index(i) => {
                let is_default_data = !prop.is_accessor() && prop.attrs == Attrs::ALL;
                let o = self.obj_mut(obj);
                match &mut o.elements {
                    Elements::Dense(v)
                        if is_default_data && i as usize <= v.len() + DENSE_GAP as usize =>
                    {
                        let idx = i as usize;
                        if idx < v.len() {
                            v[idx] = prop.value().unwrap();
                        } else {
                            if idx > v.len() {
                                v.resize(idx, Value::Empty);
                            }
                            v.push(prop.value().unwrap());
                        }
                    }
                    _ => {
                        self.make_sparse(obj);
                        let o = self.obj_mut(obj);
                        if let Elements::Sparse(m) = &mut o.elements {
                            m.insert(i, prop);
                        }
                    }
                }
                if let Class::Array(a) = &mut self.obj_mut(obj).class {
                    if i >= a.length {
                        a.length = i + 1;
                    }
                }
            }
            _ => {
                let o = self.obj(obj);
                if o.dict.is_none() && o.slots.len() >= DICT_THRESHOLD {
                    self.make_dictionary(obj);
                }
                let o = self.obj_mut(obj);
                if let Some(d) = &mut o.dict {
                    d.insert(key, prop);
                } else {
                    let shape = o.shape;
                    let new_shape = self.shape_add(shape, key, prop.attrs);
                    let o = self.obj_mut(obj);
                    o.shape = new_shape;
                    o.slots.push(prop.slot);
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
                let o = self.obj_mut(obj);
                if let Some(d) = &mut o.dict {
                    d.insert(key, prop);
                    return;
                }
                let shape = o.shape;
                let slot = self
                    .shape(shape)
                    .lookup(key)
                    .expect("replace_own_property: missing key");
                let attrs = self.shape(shape).keys[slot as usize].1;
                if attrs == prop.attrs {
                    self.obj_mut(obj).slots[slot as usize] = prop.slot;
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
                if let Some(slot) = self.shape(shape).lookup(key) {
                    self.obj_mut(obj).slots[slot as usize] = PropSlot::Data(value);
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
                let o = self.obj_mut(obj);
                if let Some(d) = &mut o.dict {
                    return d.shift_remove(&key).is_some();
                }
                let shape = o.shape;
                let Some(slot) = self.shape(shape).lookup(key) else {
                    return false;
                };
                // Removing the most recently added key walks back to the
                // parent shape; anything else goes to dictionary mode.
                let sh = self.shape(shape);
                if slot as usize + 1 == sh.keys.len() {
                    let parent = sh.parent.expect("non-root shape has a parent");
                    let o = self.obj_mut(obj);
                    o.slots.pop();
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
            for (k, _) in &self.shape(o.shape).keys {
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
                self.shape(o.shape)
                    .keys
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
        let o = self.obj_mut(obj);
        if let (Elements::Dense(v), Class::Array(a)) = (&mut o.elements, &mut o.class) {
            if v.len() == a.length as usize && a.length < PropKey::MAX_INDEX {
                v.push(value);
                a.length += 1;
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
