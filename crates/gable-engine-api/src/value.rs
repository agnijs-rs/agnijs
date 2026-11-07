//! Values crossing the engine interface.
//!
//! Primitives are copied out of the engine. Heap values (objects, functions,
//! symbols) are referenced through a [`Handle`]: a persistent root into the
//! engine's handle table that stays valid until every clone of it is dropped.
//! Handles carry no lifetime, so hosts can store them in their own structures.
//! Dropping the last clone queues the slot for release; the engine reclaims it
//! the next time the host calls into it (see [`HandleRegistry`]).

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

/// A UTF-16 JavaScript string. May contain lone surrogates, which is why it
/// is not a Rust `String`.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct JsString(Rc<[u16]>);

impl JsString {
    pub fn from_utf16(units: Vec<u16>) -> Self {
        JsString(units.into())
    }
    pub fn units(&self) -> &[u16] {
        &self.0
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// Lossy conversion: lone surrogates become U+FFFD.
    pub fn to_string_lossy(&self) -> String {
        String::from_utf16_lossy(&self.0)
    }
}

impl From<&str> for JsString {
    fn from(s: &str) -> Self {
        JsString(s.encode_utf16().collect())
    }
}
impl From<String> for JsString {
    fn from(s: String) -> Self {
        JsString::from(s.as_str())
    }
}
impl fmt::Debug for JsString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.to_string_lossy())
    }
}
impl fmt::Display for JsString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_string_lossy())
    }
}
impl PartialEq<str> for JsString {
    fn eq(&self, other: &str) -> bool {
        self.0.iter().copied().eq(other.encode_utf16())
    }
}

/// A BigInt crossing the interface, as its decimal representation.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct JsBigInt(pub String);

/// Identifies a realm created by [`crate::Engine::create_realm`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct RealmId(pub u32);

/// Which engine a handle belongs to. Handles from one engine instance must not
/// be given to another; engines check this and report [`crate::Error::ForeignHandle`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct EngineId(pub u64);

/// The kind of heap value a handle points at.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum HeapKind {
    Object,
    Symbol,
}

struct HandleInner {
    id: u32,
    engine: EngineId,
    kind: HeapKind,
    released: Rc<RefCell<Vec<u32>>>,
}

impl Drop for HandleInner {
    fn drop(&mut self) {
        self.released.borrow_mut().push(self.id);
    }
}

/// A persistent, reference-counted root to a heap value.
#[derive(Clone)]
pub struct Handle(Rc<HandleInner>);

impl Handle {
    /// The slot index in the owning engine's handle table.
    pub fn id(&self) -> u32 {
        self.0.id
    }
    pub fn engine(&self) -> EngineId {
        self.0.engine
    }
    pub fn kind(&self) -> HeapKind {
        self.0.kind
    }
}
impl fmt::Debug for Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Handle({:?}#{})", self.0.kind, self.0.id)
    }
}
/// Slot identity. Two distinct slots may still point at the same JS object;
/// use [`crate::Engine::strict_equals`] for JS identity.
impl PartialEq for Handle {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for Handle {}

/// A JavaScript value as seen by the host.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(JsString),
    BigInt(JsBigInt),
    Symbol(Handle),
    Object(Handle),
}

impl Value {
    pub fn string(s: &str) -> Value {
        Value::String(JsString::from(s))
    }
    pub fn is_undefined(&self) -> bool {
        matches!(self, Value::Undefined)
    }
    pub fn is_nullish(&self) -> bool {
        matches!(self, Value::Undefined | Value::Null)
    }
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&JsString> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_object(&self) -> Option<&Handle> {
        match self {
            Value::Object(h) => Some(h),
            _ => None,
        }
    }
    /// The handle behind an object or symbol value.
    pub fn as_handle(&self) -> Option<&Handle> {
        match self {
            Value::Object(h) | Value::Symbol(h) => Some(h),
            _ => None,
        }
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Value::Number(n)
    }
}
impl From<i32> for Value {
    fn from(n: i32) -> Self {
        Value::Number(n as f64)
    }
}
impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}
impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::string(s)
    }
}
impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(s.into())
    }
}

/// A property key: a string or a symbol. Integer indices are strings here;
/// engines canonicalise them.
#[derive(Clone, Debug, PartialEq)]
pub enum PropertyKey {
    String(JsString),
    Symbol(Handle),
}

impl From<&str> for PropertyKey {
    fn from(s: &str) -> Self {
        PropertyKey::String(s.into())
    }
}
impl From<u32> for PropertyKey {
    fn from(i: u32) -> Self {
        PropertyKey::String(i.to_string().into())
    }
}
impl From<JsString> for PropertyKey {
    fn from(s: JsString) -> Self {
        PropertyKey::String(s)
    }
}

/// The observable state of a promise.
#[derive(Clone, Debug, PartialEq)]
pub enum PromiseState {
    Pending,
    Fulfilled(Value),
    Rejected(Value),
}

/// `typeof` results.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TypeOf {
    Undefined,
    Null,
    Boolean,
    Number,
    String,
    BigInt,
    Symbol,
    Object,
    Function,
}

/// Slot storage for persistent handles. Engines embed one of these and call
/// [`HandleRegistry::reclaim`] at every entry point to free slots whose
/// handles were dropped.
pub struct HandleRegistry<T> {
    slots: Vec<Option<T>>,
    free: Vec<u32>,
    released: Rc<RefCell<Vec<u32>>>,
    engine: EngineId,
}

impl<T> HandleRegistry<T> {
    pub fn new(engine: EngineId) -> Self {
        HandleRegistry {
            slots: Vec::new(),
            free: Vec::new(),
            released: Rc::new(RefCell::new(Vec::new())),
            engine,
        }
    }

    pub fn engine_id(&self) -> EngineId {
        self.engine
    }

    /// Store a value and return a fresh handle to it.
    pub fn insert(&mut self, kind: HeapKind, value: T) -> Handle {
        let id = match self.free.pop() {
            Some(id) => {
                self.slots[id as usize] = Some(value);
                id
            }
            None => {
                self.slots.push(Some(value));
                (self.slots.len() - 1) as u32
            }
        };
        Handle(Rc::new(HandleInner {
            id,
            engine: self.engine,
            kind,
            released: self.released.clone(),
        }))
    }

    /// Look a handle up. `None` when the handle belongs to another engine.
    pub fn get(&self, handle: &Handle) -> Option<&T> {
        if handle.engine() != self.engine {
            return None;
        }
        self.slots.get(handle.id() as usize).and_then(|s| s.as_ref())
    }

    pub fn get_mut(&mut self, handle: &Handle) -> Option<&mut T> {
        if handle.engine() != self.engine {
            return None;
        }
        self.slots.get_mut(handle.id() as usize).and_then(|s| s.as_mut())
    }

    /// Free the slots of every handle dropped since the last call. Returns
    /// the freed values so engines can release engine-side references.
    pub fn reclaim(&mut self) -> Vec<T> {
        let ids: Vec<u32> = std::mem::take(&mut *self.released.borrow_mut());
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(v) = self.slots[id as usize].take() {
                out.push(v);
                self.free.push(id);
            }
        }
        out
    }

    /// Number of live slots.
    pub fn live(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    /// Iterate live values (for GC rooting).
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.slots.iter().filter_map(|s| s.as_ref())
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.slots.iter_mut().filter_map(|s| s.as_mut())
    }
}

/// Monotonic source of engine ids; one per engine instance.
pub fn next_engine_id() -> EngineId {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    EngineId(NEXT.fetch_add(1, Ordering::Relaxed))
}
