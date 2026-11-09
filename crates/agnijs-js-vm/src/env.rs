//! Environment records.
//!
//! Most bindings never reach here: the compiler keeps uncaptured bindings
//! in registers. Heap environments hold captured bindings (addressed by
//! `(depth, slot)` from bytecode), plus the names the compiler recorded so
//! dynamic lookup (`with`, direct `eval`) and the debugger can find them.

use crate::value::{EnvId, ObjId, Value};
use agnijs_engine_api::RealmId;
use agnijs_js_compiler::bytecode::{BindingKind, CodeBlock, ScopeInfo};
use indexmap::IndexMap;
use std::rc::Rc;

pub enum Environment {
    Declarative(DeclEnv),
    /// `with (obj)`, or the global object record behind the global
    /// environment (the latter is modelled by `Global`).
    Object(ObjectEnv),
    /// The global environment of a realm: the realm's lexical declarations
    /// and the global object.
    Global(RealmId),
}

pub struct DeclEnv {
    pub slots: Vec<Value>,
    /// The scope that describes `slots` (names and kinds), when the
    /// environment came from bytecode.
    pub scope: Option<(Rc<CodeBlock>, u32)>,
    /// Bindings added by sloppy direct `eval` (`var` and function
    /// declarations land in the nearest var scope) or by eval's own
    /// lexical declarations.
    pub extra: Option<Box<IndexMap<Rc<str>, ExtraBinding>>>,
    pub parent: Option<EnvId>,
    /// A function-level scope: where sloppy eval `var`s are created.
    pub is_var_scope: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct ExtraBinding {
    pub value: Value,
    pub kind: BindingKind,
    /// Created by eval and therefore deletable (`delete x`).
    pub deletable: bool,
}

pub struct ObjectEnv {
    pub object: ObjId,
    /// A `with` environment (checks `@@unscopables`).
    pub with: bool,
    pub parent: Option<EnvId>,
}

impl Environment {
    pub fn declarative(scope: Option<(Rc<CodeBlock>, u32)>, parent: Option<EnvId>) -> Environment {
        Environment::declarative_in(Vec::new(), scope, parent)
    }

    /// [`Environment::declarative`] with its bindings stored in `slots`,
    /// an empty buffer (`Heap::alloc_declarative` passes a recycled one).
    pub fn declarative_in(
        mut slots: Vec<Value>,
        scope: Option<(Rc<CodeBlock>, u32)>,
        parent: Option<EnvId>,
    ) -> Environment {
        debug_assert!(slots.is_empty());
        let is_var_scope = match &scope {
            Some((code, idx)) => {
                let info: &ScopeInfo = &code.scopes[*idx as usize];
                slots.extend(info.bindings.iter().map(|(_, kind)| {
                    if kind.starts_empty() {
                        Value::Empty
                    } else {
                        Value::Undefined
                    }
                }));
                info.is_var_scope
            }
            None => false,
        };
        Environment::Declarative(DeclEnv {
            slots,
            scope,
            extra: None,
            parent,
            is_var_scope,
        })
    }

    /// A synthetic declarative environment with `n` undefined slots.
    pub fn synthetic(n: usize, parent: Option<EnvId>, is_var_scope: bool) -> Environment {
        Environment::Declarative(DeclEnv {
            slots: vec![Value::Undefined; n],
            scope: None,
            extra: None,
            parent,
            is_var_scope,
        })
    }

    pub fn with_object(object: ObjId, parent: Option<EnvId>) -> Environment {
        Environment::Object(ObjectEnv {
            object,
            with: true,
            parent,
        })
    }

    pub fn parent(&self) -> Option<EnvId> {
        match self {
            Environment::Declarative(d) => d.parent,
            Environment::Object(o) => o.parent,
            Environment::Global(_) => None,
        }
    }

    pub fn slot_count(&self) -> usize {
        match self {
            Environment::Declarative(d) => {
                d.slots.len() + d.extra.as_ref().map(|e| e.len()).unwrap_or(0)
            }
            _ => 0,
        }
    }

    pub fn as_decl(&self) -> Option<&DeclEnv> {
        match self {
            Environment::Declarative(d) => Some(d),
            _ => None,
        }
    }

    pub fn as_decl_mut(&mut self) -> Option<&mut DeclEnv> {
        match self {
            Environment::Declarative(d) => Some(d),
            _ => None,
        }
    }

    pub fn is_var_scope(&self) -> bool {
        match self {
            Environment::Declarative(d) => d.is_var_scope,
            _ => false,
        }
    }

    pub fn scope_info(&self) -> Option<&ScopeInfo> {
        match self {
            Environment::Declarative(DeclEnv {
                scope: Some((code, idx)),
                ..
            }) => Some(&code.scopes[*idx as usize]),
            _ => None,
        }
    }

    /// Clone a declarative environment's bindings (per-iteration copies).
    pub fn copy_declarative(&self) -> Environment {
        match self {
            Environment::Declarative(d) => Environment::Declarative(DeclEnv {
                slots: d.slots.clone(),
                scope: d.scope.clone(),
                extra: d.extra.clone(),
                parent: d.parent,
                is_var_scope: d.is_var_scope,
            }),
            Environment::Object(o) => Environment::Object(ObjectEnv {
                object: o.object,
                with: o.with,
                parent: o.parent,
            }),
            Environment::Global(r) => Environment::Global(*r),
        }
    }
}

impl DeclEnv {
    /// Find a binding by name among the scope's slots.
    pub fn slot_of(&self, name: &str) -> Option<(usize, BindingKind)> {
        let (code, idx) = self.scope.as_ref()?;
        let info = &code.scopes[*idx as usize];
        info.bindings
            .iter()
            .position(|(n, _)| &**n == name)
            .map(|i| (i, info.bindings[i].1))
    }

    pub fn extra_get(&self, name: &str) -> Option<&ExtraBinding> {
        self.extra.as_ref()?.get(name)
    }

    pub fn extra_insert(&mut self, name: Rc<str>, b: ExtraBinding) {
        self.extra
            .get_or_insert_with(Default::default)
            .insert(name, b);
    }
}
