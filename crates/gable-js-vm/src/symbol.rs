//! Symbols and private names.

use crate::value::{StrId, Value};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SymbolKind {
    /// `Symbol()` / `Symbol(desc)`.
    Normal,
    /// `Symbol.for(key)`: in the global registry, never collected.
    Registered,
    /// `Symbol.iterator` and friends, shared by every realm.
    WellKnown,
    /// A class private name (`#x`). Never visible to reflection.
    Private,
}

/// What a private name refers to once its class has been evaluated.
#[derive(Clone, Copy, Debug)]
pub enum PrivateElement {
    /// A field: each instance stores its own value.
    Field,
    /// A method shared by every instance.
    Method(Value),
    /// An accessor pair (each half `Undefined` when absent).
    Accessor { get: Value, set: Value },
}

pub struct SymbolData {
    pub description: Option<StrId>,
    pub kind: SymbolKind,
    /// For private names: the element kind (methods and accessors carry
    /// their functions here; `AddPrivateBrand` installs them).
    pub private: PrivateElement,
}

impl SymbolData {
    pub fn new(description: Option<StrId>, kind: SymbolKind) -> SymbolData {
        SymbolData {
            description,
            kind,
            private: PrivateElement::Field,
        }
    }
    pub fn is_private(&self) -> bool {
        self.kind == SymbolKind::Private
    }
}
