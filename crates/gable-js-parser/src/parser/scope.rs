//! Declaration tracking for early errors: duplicate lexical declarations,
//! `var`/`let` conflicts, parameter clashes, catch-parameter rules and
//! private-name resolution in classes.

use crate::error::{Result, SyntaxError};
use crate::span::Span;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LexKind {
    /// `let`, `const`, `class`, or an import binding.
    Lexical,
    /// A function declaration in a block (Annex B allows duplicates in
    /// sloppy code).
    BlockFunction,
    /// A simple catch parameter (`catch (e)`): `var e` is allowed in the
    /// body (Annex B), except through `for (var e of ...)`.
    CatchParamSimple,
    /// A destructured catch parameter: no `var` of the same name allowed.
    CatchParamPattern,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScopeKind {
    /// Script, function body, class static block or module top level.
    FunctionTop { module: bool },
    Block,
    Catch,
}

#[derive(Debug)]
pub(crate) struct Scope {
    pub kind: ScopeKind,
    lexical: HashMap<Rc<str>, LexKind>,
    /// `var` names declared in or below this scope.
    vars: HashSet<Rc<str>>,
    /// Parameter names (function-top scopes only).
    params: HashSet<Rc<str>>,
    /// Function declarations at the top level of a function or script
    /// (var-like, but they conflict with lexical declarations).
    top_functions: HashSet<Rc<str>>,
}

impl Scope {
    fn new(kind: ScopeKind) -> Scope {
        Scope { kind, lexical: HashMap::new(), vars: HashSet::new(), params: HashSet::new(), top_functions: HashSet::new() }
    }
}

#[derive(Debug, Default)]
pub(crate) struct ScopeStack {
    scopes: Vec<Scope>,
}

impl ScopeStack {
    pub fn new() -> ScopeStack {
        ScopeStack::default()
    }

    pub fn push_function_top(&mut self, module: bool) {
        self.scopes.push(Scope::new(ScopeKind::FunctionTop { module }));
    }

    pub fn push_block(&mut self) {
        self.scopes.push(Scope::new(ScopeKind::Block));
    }

    pub fn push_catch(&mut self) {
        self.scopes.push(Scope::new(ScopeKind::Catch));
    }

    pub fn pop(&mut self) {
        self.scopes.pop();
    }

    fn current(&mut self) -> &mut Scope {
        self.scopes.last_mut().expect("scope stack is never empty while parsing")
    }

    /// Record a parameter name in the enclosing function-top scope.
    pub fn add_param(&mut self, name: Rc<str>) {
        self.current().params.insert(name);
    }

    pub fn declare_catch_param(&mut self, name: Rc<str>, span: Span, simple: bool) -> Result<()> {
        let scope = self.current();
        let kind = if simple { LexKind::CatchParamSimple } else { LexKind::CatchParamPattern };
        if scope.lexical.insert(name.clone(), kind).is_some() {
            return Err(SyntaxError::new(format!("duplicate catch parameter '{name}'"), span));
        }
        Ok(())
    }

    /// Declare a `let`/`const`/`class`/import binding (or a block-level
    /// function declaration) in the current scope.
    pub fn declare_lexical(&mut self, name: Rc<str>, span: Span, kind: LexKind, strict: bool) -> Result<()> {
        if &*name == "let" && kind != LexKind::BlockFunction {
            return Err(SyntaxError::new("'let' cannot be a lexically bound name", span));
        }
        let scope = self.current();
        if let Some(existing) = scope.lexical.get(&name) {
            let both_functions = *existing == LexKind::BlockFunction && kind == LexKind::BlockFunction;
            if !(both_functions && !strict && scope.kind == ScopeKind::Block) {
                return Err(SyntaxError::new(format!("identifier '{name}' has already been declared"), span));
            }
        }
        if scope.vars.contains(&name) || scope.top_functions.contains(&name) || scope.params.contains(&name) {
            return Err(SyntaxError::new(format!("identifier '{name}' has already been declared"), span));
        }
        scope.lexical.insert(name, kind);
        Ok(())
    }

    /// Declare a `var` binding: it hoists to the nearest function-top scope
    /// and must not collide with a lexical declaration on the way.
    pub fn declare_var(&mut self, name: Rc<str>, span: Span) -> Result<()> {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(kind) = scope.lexical.get(&name) {
                let allowed = *kind == LexKind::CatchParamSimple;
                if !allowed {
                    return Err(SyntaxError::new(format!("identifier '{name}' has already been declared"), span));
                }
            }
            scope.vars.insert(name.clone());
            if matches!(scope.kind, ScopeKind::FunctionTop { .. }) {
                break;
            }
        }
        Ok(())
    }

    /// Declare a function declaration where the current scope is a
    /// function top (script, function body, static block). In a module's
    /// top level and in blocks, function declarations are lexical.
    /// `plain` is false for generators and async functions, which never
    /// get the Annex B duplicate allowance in blocks.
    pub fn declare_function(&mut self, name: Rc<str>, span: Span, strict: bool, plain: bool) -> Result<()> {
        let scope = self.current();
        match scope.kind {
            ScopeKind::FunctionTop { module: false } => {
                if scope.lexical.contains_key(&name) {
                    return Err(SyntaxError::new(format!("identifier '{name}' has already been declared"), span));
                }
                scope.top_functions.insert(name);
                Ok(())
            }
            ScopeKind::FunctionTop { module: true } => self.declare_lexical(name, span, LexKind::Lexical, strict),
            ScopeKind::Block | ScopeKind::Catch => {
                let kind = if plain { LexKind::BlockFunction } else { LexKind::Lexical };
                self.declare_lexical(name, span, kind, strict)
            }
        }
    }
}

/// Private names declared and referenced by a class body.
#[derive(Debug, Default)]
pub(crate) struct PrivateEnv {
    declared: HashMap<Rc<str>, PrivateDecl>,
    pub referenced: Vec<(Rc<str>, Span)>,
}

#[derive(Debug, Clone, Copy, Default)]
struct PrivateDecl {
    getter: bool,
    setter: bool,
    other: bool,
    is_static: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrivateKind {
    Getter,
    Setter,
    Other,
}

impl PrivateEnv {
    pub fn declare(&mut self, name: Rc<str>, span: Span, kind: PrivateKind, is_static: bool) -> Result<()> {
        let dup = || Err(SyntaxError::new(format!("duplicate private name #{name}"), span));
        let entry = self.declared.entry(name.clone()).or_default();
        let fresh = !entry.getter && !entry.setter && !entry.other;
        if fresh {
            entry.is_static = is_static;
        } else if entry.is_static != is_static || entry.other {
            return dup();
        }
        match kind {
            PrivateKind::Getter => {
                if entry.getter {
                    return dup();
                }
                entry.getter = true;
            }
            PrivateKind::Setter => {
                if entry.setter {
                    return dup();
                }
                entry.setter = true;
            }
            PrivateKind::Other => {
                if !fresh {
                    return dup();
                }
                entry.other = true;
            }
        }
        Ok(())
    }

    pub fn is_declared(&self, name: &str) -> bool {
        self.declared.contains_key(name)
    }
}
