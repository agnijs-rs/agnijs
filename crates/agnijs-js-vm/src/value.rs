//! The value representation.
//!
//! A [`Value`] is a small `Copy` enum: primitives inline, heap values as
//! arena indices. Heap indices are only meaningful together with the
//! [`crate::heap::Heap`] that issued them, and only while the value is
//! reachable from a root (frames, the root stack, handles, realms).

use std::fmt;

macro_rules! id_type {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub u32);
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}#{}", stringify!($name), self.0)
            }
        }
        impl $name {
            #[inline]
            pub fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

id_type!(
    /// An object in the heap.
    ObjId
);
id_type!(
    /// A string in the heap.
    StrId
);
id_type!(
    /// A symbol (including private names) in the heap.
    SymId
);
id_type!(
    /// A BigInt in the heap.
    BigId
);
id_type!(
    /// An environment record in the heap.
    EnvId
);
id_type!(
    /// A shape (hidden class).
    ShapeId
);

/// A JavaScript value, or the hole.
#[derive(Clone, Copy, PartialEq)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(StrId),
    Symbol(SymId),
    BigInt(BigId),
    Object(ObjId),
    /// The hole: an uninitialised binding (TDZ) or a missing array element.
    /// Never observable from JavaScript.
    Empty,
}

impl Value {
    #[inline]
    pub fn is_undefined(self) -> bool {
        matches!(self, Value::Undefined)
    }
    #[inline]
    pub fn is_null(self) -> bool {
        matches!(self, Value::Null)
    }
    #[inline]
    pub fn is_nullish(self) -> bool {
        matches!(self, Value::Undefined | Value::Null)
    }
    #[inline]
    pub fn is_empty(self) -> bool {
        matches!(self, Value::Empty)
    }
    #[inline]
    pub fn is_object(self) -> bool {
        matches!(self, Value::Object(_))
    }
    #[inline]
    pub fn as_object(self) -> Option<ObjId> {
        match self {
            Value::Object(o) => Some(o),
            _ => None,
        }
    }
    #[inline]
    pub fn as_number(self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(n),
            _ => None,
        }
    }
    #[inline]
    pub fn as_string(self) -> Option<StrId> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }
    #[inline]
    pub fn as_bool(self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(b),
            _ => None,
        }
    }
    #[inline]
    pub fn is_string(self) -> bool {
        matches!(self, Value::String(_))
    }
    #[inline]
    pub fn is_symbol(self) -> bool {
        matches!(self, Value::Symbol(_))
    }
    #[inline]
    pub fn is_number(self) -> bool {
        matches!(self, Value::Number(_))
    }
    #[inline]
    pub fn is_bigint(self) -> bool {
        matches!(self, Value::BigInt(_))
    }
    #[inline]
    pub fn int(i: i32) -> Value {
        Value::Number(i as f64)
    }
    #[inline]
    pub fn from_u32(i: u32) -> Value {
        Value::Number(i as f64)
    }
    #[inline]
    pub fn from_usize(i: usize) -> Value {
        Value::Number(i as f64)
    }
    /// Replace the hole with undefined.
    #[inline]
    pub fn or_undefined(self) -> Value {
        if self.is_empty() {
            Value::Undefined
        } else {
            self
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Undefined => f.write_str("undefined"),
            Value::Null => f.write_str("null"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Number(n) => write!(f, "{n}"),
            Value::String(s) => write!(f, "{s:?}"),
            Value::Symbol(s) => write!(f, "{s:?}"),
            Value::BigInt(b) => write!(f, "{b:?}"),
            Value::Object(o) => write!(f, "{o:?}"),
            Value::Empty => f.write_str("<empty>"),
        }
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
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
impl From<u32> for Value {
    fn from(n: u32) -> Self {
        Value::Number(n as f64)
    }
}
impl From<ObjId> for Value {
    fn from(o: ObjId) -> Self {
        Value::Object(o)
    }
}
impl From<StrId> for Value {
    fn from(s: StrId) -> Self {
        Value::String(s)
    }
}
impl From<SymId> for Value {
    fn from(s: SymId) -> Self {
        Value::Symbol(s)
    }
}

/// A property key: an array index, an interned string (atom) or a symbol.
///
/// Array indices (`0 ..= 2^32 - 2`) are kept numeric so element storage
/// never needs string comparisons. Every string key is an atom, so key
/// equality is id equality.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PropKey {
    Index(u32),
    Str(StrId),
    Sym(SymId),
}

impl PropKey {
    pub const MAX_INDEX: u32 = u32::MAX - 1;

    #[inline]
    pub fn is_symbol(self) -> bool {
        matches!(self, PropKey::Sym(_))
    }
    #[inline]
    pub fn as_index(self) -> Option<u32> {
        match self {
            PropKey::Index(i) => Some(i),
            _ => None,
        }
    }
    /// Parse a canonical array index from UTF-16 units.
    pub fn index_from_units(units: &[u16]) -> Option<u32> {
        if units.is_empty() || units.len() > 10 {
            return None;
        }
        if units.len() > 1 && units[0] == b'0' as u16 {
            return None;
        }
        let mut v: u64 = 0;
        for &u in units {
            if !(b'0' as u16..=b'9' as u16).contains(&u) {
                return None;
            }
            v = v * 10 + (u - b'0' as u16) as u64;
        }
        if v <= Self::MAX_INDEX as u64 {
            Some(v as u32)
        } else {
            None
        }
    }
    /// Parse a canonical array index from bytes (one-byte strings / `str`).
    pub fn index_from_bytes(bytes: &[u8]) -> Option<u32> {
        if bytes.is_empty() || bytes.len() > 10 {
            return None;
        }
        if bytes.len() > 1 && bytes[0] == b'0' {
            return None;
        }
        let mut v: u64 = 0;
        for &b in bytes {
            if !b.is_ascii_digit() {
                return None;
            }
            v = v * 10 + (b - b'0') as u64;
        }
        if v <= Self::MAX_INDEX as u64 {
            Some(v as u32)
        } else {
            None
        }
    }
}

/// Property attributes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Attrs(pub u8);

impl Attrs {
    pub const WRITABLE: u8 = 1;
    pub const ENUMERABLE: u8 = 2;
    pub const CONFIGURABLE: u8 = 4;
    /// Writable, enumerable, configurable: what assignment creates.
    pub const ALL: Attrs = Attrs(7);
    /// Writable and configurable but not enumerable: built-in methods.
    pub const METHOD: Attrs = Attrs(5);
    /// Nothing set: frozen-style constants.
    pub const NONE: Attrs = Attrs(0);
    /// Configurable only (accessors on built-ins and class methods).
    pub const CONFIGURABLE_ONLY: Attrs = Attrs(4);

    #[inline]
    pub fn writable(self) -> bool {
        self.0 & Self::WRITABLE != 0
    }
    #[inline]
    pub fn enumerable(self) -> bool {
        self.0 & Self::ENUMERABLE != 0
    }
    #[inline]
    pub fn configurable(self) -> bool {
        self.0 & Self::CONFIGURABLE != 0
    }
    #[inline]
    pub fn with_writable(self, w: bool) -> Attrs {
        Attrs(if w {
            self.0 | Self::WRITABLE
        } else {
            self.0 & !Self::WRITABLE
        })
    }
    #[inline]
    pub fn with_enumerable(self, e: bool) -> Attrs {
        Attrs(if e {
            self.0 | Self::ENUMERABLE
        } else {
            self.0 & !Self::ENUMERABLE
        })
    }
    #[inline]
    pub fn with_configurable(self, c: bool) -> Attrs {
        Attrs(if c {
            self.0 | Self::CONFIGURABLE
        } else {
            self.0 & !Self::CONFIGURABLE
        })
    }
    pub fn new(writable: bool, enumerable: bool, configurable: bool) -> Attrs {
        Attrs::NONE
            .with_writable(writable)
            .with_enumerable(enumerable)
            .with_configurable(configurable)
    }
}

/// One half of an accessor pair: a function object or none (`undefined`),
/// in four bytes, so a [`PropSlot`] is no bigger than a [`Value`]
/// (TASK-100.21).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OptObj(u32);

impl OptObj {
    pub const NONE: OptObj = OptObj(u32::MAX);

    /// From a getter or setter value: an object or `undefined` (property
    /// descriptors validate this before any accessor is stored).
    #[inline]
    pub fn from_value(v: Value) -> OptObj {
        match v {
            Value::Object(o) => {
                debug_assert!(o.0 != u32::MAX);
                OptObj(o.0)
            }
            _ => {
                debug_assert!(v.is_undefined(), "accessor half that is not an object: {v:?}");
                OptObj::NONE
            }
        }
    }

    /// The half as a value: the function, or `undefined`.
    #[inline]
    pub fn value(self) -> Value {
        if self.0 == u32::MAX {
            Value::Undefined
        } else {
            Value::Object(ObjId(self.0))
        }
    }

    #[inline]
    pub fn is_undefined(self) -> bool {
        self.0 == u32::MAX
    }
}

/// A property's value part: a data value or an accessor pair (each half
/// none when absent). Sixteen bytes, the size of a [`Value`].
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PropSlot {
    Data(Value),
    Accessor { get: OptObj, set: OptObj },
}

impl PropSlot {
    /// An empty slot (`undefined` data), the filler of unused inline slots.
    pub const UNDEFINED: PropSlot = PropSlot::Data(Value::Undefined);

    /// An accessor pair from two values (objects or `undefined`).
    #[inline]
    pub fn accessor(get: Value, set: Value) -> PropSlot {
        PropSlot::Accessor {
            get: OptObj::from_value(get),
            set: OptObj::from_value(set),
        }
    }
}

/// A full own property.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Property {
    pub slot: PropSlot,
    pub attrs: Attrs,
}

impl Property {
    pub fn data(value: Value, attrs: Attrs) -> Property {
        Property {
            slot: PropSlot::Data(value),
            attrs,
        }
    }
    pub fn accessor(get: Value, set: Value, attrs: Attrs) -> Property {
        Property {
            slot: PropSlot::accessor(get, set),
            attrs,
        }
    }
    pub fn is_accessor(&self) -> bool {
        matches!(self.slot, PropSlot::Accessor { .. })
    }
    pub fn value(&self) -> Option<Value> {
        match self.slot {
            PropSlot::Data(v) => Some(v),
            _ => None,
        }
    }
}

/// A property descriptor with optional fields (the spec's Property
/// Descriptor record).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PropertyDescriptor {
    pub value: Option<Value>,
    pub get: Option<Value>,
    pub set: Option<Value>,
    pub writable: Option<bool>,
    pub enumerable: Option<bool>,
    pub configurable: Option<bool>,
}

impl PropertyDescriptor {
    pub fn is_accessor(&self) -> bool {
        self.get.is_some() || self.set.is_some()
    }
    pub fn is_data(&self) -> bool {
        self.value.is_some() || self.writable.is_some()
    }
    pub fn is_generic(&self) -> bool {
        !self.is_accessor() && !self.is_data()
    }
    pub fn data(value: Value, attrs: Attrs) -> PropertyDescriptor {
        PropertyDescriptor {
            value: Some(value),
            writable: Some(attrs.writable()),
            enumerable: Some(attrs.enumerable()),
            configurable: Some(attrs.configurable()),
            ..Default::default()
        }
    }
    pub fn from_property(p: &Property) -> PropertyDescriptor {
        match p.slot {
            PropSlot::Data(v) => PropertyDescriptor {
                value: Some(v),
                writable: Some(p.attrs.writable()),
                enumerable: Some(p.attrs.enumerable()),
                configurable: Some(p.attrs.configurable()),
                ..Default::default()
            },
            PropSlot::Accessor { get, set } => PropertyDescriptor {
                get: Some(get.value()),
                set: Some(set.value()),
                enumerable: Some(p.attrs.enumerable()),
                configurable: Some(p.attrs.configurable()),
                ..Default::default()
            },
        }
    }
    /// Fill in defaults and build a concrete property (for new properties).
    pub fn to_property(&self) -> Property {
        let attrs = Attrs::new(
            self.writable.unwrap_or(false),
            self.enumerable.unwrap_or(false),
            self.configurable.unwrap_or(false),
        );
        if self.is_accessor() {
            Property::accessor(
                self.get.unwrap_or(Value::Undefined),
                self.set.unwrap_or(Value::Undefined),
                attrs.with_writable(false),
            )
        } else {
            Property::data(self.value.unwrap_or(Value::Undefined), attrs)
        }
    }
}

impl Attrs {
    /// Attributes from the three flags.
    pub const fn from_flags(writable: bool, enumerable: bool, configurable: bool) -> Attrs {
        Attrs((writable as u8) | ((enumerable as u8) << 1) | ((configurable as u8) << 2))
    }
    /// Writable only (RegExp `lastIndex`).
    pub const WRITABLE_ONLY: Attrs = Attrs(1);
}
