//! Global and dynamic (by-name) binding access, declaration instantiation
//! for scripts and eval code, and direct eval.

use super::{ConstructKind, Frame, FrameKind, RunOutcome};
use crate::env::{Environment, ExtraBinding};
use crate::realm::LexBinding;
use crate::value::{Attrs, EnvId, ObjId, PropKey, PropSlot, Property, Value};
use crate::vm::{Fault, JsResult, Vm};
use agnijs_js_compiler::bytecode::{BindingKind, CodeBlock, CodeKind, GlobalEntry, ScopeKind};
use agnijs_js_compiler::EvalOptions;
use std::rc::Rc;

/// Where a name resolved to.
#[derive(Clone, Copy, Debug)]
pub enum Binding {
    Slot {
        env: EnvId,
        slot: usize,
        kind: BindingKind,
    },
    Extra {
        env: EnvId,
    },
    /// A `with` object (or other object environment) that has the property.
    Object {
        obj: ObjId,
        with: bool,
    },
    GlobalLexical,
    GlobalProperty,
    Unresolvable,
}

impl Vm {
    // ---- resolution --------------------------------------------------------------

    /// ResolveBinding from `start` outwards.
    pub(crate) fn resolve_binding(
        &mut self,
        start: Option<EnvId>,
        name: &str,
    ) -> JsResult<Binding> {
        let mut cur = start;
        while let Some(e) = cur {
            match self.heap.env(e) {
                Environment::Declarative(d) => {
                    if let Some((slot, kind)) = d.slot_of(name) {
                        return Ok(Binding::Slot { env: e, slot, kind });
                    }
                    if d.extra_get(name).is_some() {
                        return Ok(Binding::Extra { env: e });
                    }
                    cur = d.parent;
                }
                Environment::Object(o) => {
                    let obj = o.object;
                    let with = o.with;
                    let parent = o.parent;
                    let key = self.key(name);
                    if self.has_property(obj, key)? {
                        if with {
                            // @@unscopables
                            let uns = self.get_prop(
                                obj,
                                PropKey::Sym(self.well_known.unscopables),
                                Value::Object(obj),
                            )?;
                            if let Value::Object(u) = uns {
                                let blocked = self.get_prop(u, key, uns)?;
                                if self.to_boolean(blocked) {
                                    cur = parent;
                                    continue;
                                }
                            }
                        }
                        return Ok(Binding::Object { obj, with });
                    }
                    cur = parent;
                }
                Environment::Global(realm) => {
                    let realm = *realm;
                    if self.realms[realm.0 as usize]
                        .global_lexicals
                        .contains_key(name)
                    {
                        return Ok(Binding::GlobalLexical);
                    }
                    let global = self.realms[realm.0 as usize].global;
                    let key = self.key(name);
                    if self.has_property(global, key)? {
                        return Ok(Binding::GlobalProperty);
                    }
                    return Ok(Binding::Unresolvable);
                }
            }
        }
        Ok(Binding::Unresolvable)
    }

    fn tdz_error(&mut self, name: &str) -> Fault {
        let n = match name {
            "*this*" => "this",
            other => other,
        };
        if n == "this" {
            self.throw_reference("Must call super constructor in derived class before accessing 'this' or returning from derived constructor")
        } else {
            self.throw_reference(&format!("Cannot access '{n}' before initialization"))
        }
    }

    fn not_defined(&mut self, name: &str) -> Fault {
        self.throw_reference(&format!("{name} is not defined"))
    }

    // ---- globals -------------------------------------------------------------------
    //
    // A global name resolves to a global lexical (`let`, `const`, `class`
    // at script top level) or to a property of the global object, which
    // is in dictionary mode. Both tables are IndexMaps, so a site caches
    // the *position* of its binding in `CodeBlock::globals` and a hit
    // does no interning and no hashing. Positions in `global_lexicals`
    // are stable because bindings are only ever appended; a position in
    // the global object's dictionary is stable until a property is
    // removed, which bumps the realm's epoch, as does declaring a lexical
    // (it may shadow a cached property). A hit re-reads the binding's
    // kind, so redefinition as an accessor, a read-only property, a
    // `const` or a binding in its TDZ falls through to the generic path
    // without any invalidation.

    #[inline]
    pub(crate) fn load_global(
        &mut self,
        code: &Rc<CodeBlock>,
        name: u32,
        or_undefined: bool,
    ) -> JsResult<Value> {
        if let Some(v) = self.load_global_cached(code, name) {
            return Ok(v);
        }
        self.load_global_slow(code, name, or_undefined)
    }

    /// The cached read of a global binding, when the entry for `name` still
    /// applies: no interning, no hashing, no allocation and no fault (the
    /// dispatch loop's fast path).
    #[inline]
    pub(crate) fn load_global_cached(&self, code: &CodeBlock, name: u32) -> Option<Value> {
        let realm = self.current_realm;
        let e = code.globals()[name as usize].get();
        if e.realm != realm.0 {
            return None;
        }
        let r = &self.realms[realm.0 as usize];
        if e.kind == GlobalEntry::LEXICAL {
            if let Some((_, b)) = r.global_lexicals.get_index(e.index as usize) {
                if !b.value.is_empty() {
                    return Some(b.value);
                }
            }
        } else if e.kind == GlobalEntry::PROPERTY && e.epoch == r.epoch {
            if let Some(d) = &self.heap.obj(r.global).dict {
                if let Some((_, p)) = d.get_index(e.index as usize) {
                    if let PropSlot::Data(v) = p.slot {
                        return Some(v);
                    }
                }
            }
        }
        None
    }

    #[inline(never)]
    fn load_global_slow(
        &mut self,
        code: &Rc<CodeBlock>,
        name: u32,
        or_undefined: bool,
    ) -> JsResult<Value> {
        let n: &Rc<str> = &code.names[name as usize];
        let realm = self.current_realm;
        if &**n == "*this*" {
            return Ok(Value::Object(self.realm().global));
        }
        if let Some((i, _, b)) = self.realms[realm.0 as usize].global_lexicals.get_full(&**n) {
            let b = *b;
            self.cache_global_lexical(code, name, i);
            if b.value.is_empty() {
                let n = n.clone();
                return Err(self.tdz_error(&n));
            }
            return Ok(b.value);
        }
        let global = self.realm().global;
        let key = self.key(n);
        if let Some(p) = self.heap.get_own_property(global, key) {
            if let Some(v) = p.value() {
                self.cache_global_property(code, name, key);
                return Ok(v);
            }
        }
        if self.has_property(global, key)? {
            return self.get_prop(global, key, Value::Object(global));
        }
        if or_undefined {
            return Ok(Value::Undefined);
        }
        let n = n.clone();
        Err(self.not_defined(&n))
    }

    #[inline]
    pub(crate) fn store_global(
        &mut self,
        code: &Rc<CodeBlock>,
        name: u32,
        v: Value,
        strict: bool,
    ) -> JsResult<()> {
        if self.store_global_cached(code, name, v) {
            return Ok(());
        }
        self.store_global_slow(code, name, v, strict)
    }

    /// The cached write of a global binding; `true` when done. Like
    /// [`Self::load_global_cached`], it never allocates or faults.
    #[inline]
    pub(crate) fn store_global_cached(&mut self, code: &CodeBlock, name: u32, v: Value) -> bool {
        let realm = self.current_realm;
        let e = code.globals()[name as usize].get();
        if e.realm != realm.0 {
            return false;
        }
        let r = &mut self.realms[realm.0 as usize];
        if e.kind == GlobalEntry::LEXICAL {
            if let Some((_, b)) = r.global_lexicals.get_index_mut(e.index as usize) {
                if !b.value.is_empty() && !b.kind.is_const() {
                    b.value = v;
                    return true;
                }
            }
        } else if e.kind == GlobalEntry::PROPERTY && e.epoch == r.epoch {
            let global = r.global;
            if let Some(d) = &mut self.heap.obj_mut(global).dict {
                if let Some((_, p)) = d.get_index_mut(e.index as usize) {
                    if p.attrs.writable() {
                        if let PropSlot::Data(slot) = &mut p.slot {
                            *slot = v;
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    #[inline(never)]
    fn store_global_slow(
        &mut self,
        code: &Rc<CodeBlock>,
        name: u32,
        v: Value,
        strict: bool,
    ) -> JsResult<()> {
        let n: Rc<str> = code.names[name as usize].clone();
        let realm = self.current_realm;
        if let Some((i, _, b)) = self.realms[realm.0 as usize].global_lexicals.get_full_mut(&*n) {
            if b.value.is_empty() {
                return Err(self.tdz_error(&n));
            }
            if b.kind.is_const() {
                return Err(self.throw_type(&format!("Assignment to constant variable '{n}'")));
            }
            b.value = v;
            self.cache_global_lexical(code, name, i);
            return Ok(());
        }
        let global = self.realm().global;
        let key = self.key(&n);
        if let Some(p) = self.heap.get_own_property(global, key) {
            if let PropSlot::Data(_) = p.slot {
                if p.attrs.writable() {
                    self.heap.set_own_value(global, key, v);
                    self.cache_global_property(code, name, key);
                    return Ok(());
                }
            }
        } else if strict && !self.has_property(global, key)? {
            return Err(self.not_defined(&n));
        }
        let ok = self.set_prop(global, key, v, Value::Object(global))?;
        if !ok && strict {
            return Err(self.throw_type(&format!(
                "Cannot assign to read only property '{n}' of object"
            )));
        }
        Ok(())
    }

    /// Remember that `name` is the global lexical at position `index`.
    fn cache_global_lexical(&self, code: &CodeBlock, name: u32, index: usize) {
        code.globals()[name as usize].set(GlobalEntry {
            kind: GlobalEntry::LEXICAL,
            realm: self.current_realm.0,
            index: index as u32,
            epoch: 0,
        });
    }

    /// Remember where `name` sits in the global object's dictionary, when
    /// it is one (a global object always is after the builtins install).
    fn cache_global_property(&self, code: &CodeBlock, name: u32, key: PropKey) {
        let r = self.realm();
        let Some(d) = &self.heap.obj(r.global).dict else {
            return;
        };
        if let Some(index) = d.get_index_of(&key) {
            code.globals()[name as usize].set(GlobalEntry {
                kind: GlobalEntry::PROPERTY,
                realm: r.id.0,
                index: index as u32,
                epoch: r.epoch,
            });
        }
    }

    // ---- dynamic ---------------------------------------------------------------------

    pub(crate) fn load_dynamic(
        &mut self,
        fi: usize,
        code: &Rc<CodeBlock>,
        name: u32,
        or_undefined: bool,
    ) -> JsResult<Value> {
        let n: Rc<str> = code.names[name as usize].clone();
        let env = self.frames[fi].env;
        match self.resolve_binding(env, &n)? {
            Binding::Slot { env, slot, .. } => {
                let v = self.heap.env(env).as_decl().unwrap().slots[slot];
                if v.is_empty() {
                    return Err(self.tdz_error(&n));
                }
                Ok(v)
            }
            Binding::Extra { env } => {
                let v = self
                    .heap
                    .env(env)
                    .as_decl()
                    .unwrap()
                    .extra_get(&n)
                    .unwrap()
                    .value;
                if v.is_empty() {
                    return Err(self.tdz_error(&n));
                }
                Ok(v)
            }
            Binding::Object { obj, with } => {
                let key = self.key(&n);
                if with && !self.has_property(obj, key)? {
                    // The property vanished between HasBinding and GetBindingValue.
                    if or_undefined || !code.strict {
                        return Ok(Value::Undefined);
                    }
                    return Err(self.not_defined(&n));
                }
                self.get_prop(obj, key, Value::Object(obj))
            }
            Binding::GlobalLexical => {
                let realm = self.frames[fi].realm;
                let v = self.realms[realm.0 as usize].global_lexicals[&*n].value;
                if v.is_empty() {
                    return Err(self.tdz_error(&n));
                }
                Ok(v)
            }
            Binding::GlobalProperty => {
                let global = self.realm_of(self.frames[fi].realm).global;
                let key = self.key(&n);
                self.get_prop(global, key, Value::Object(global))
            }
            Binding::Unresolvable => {
                if &*n == "*this*" {
                    return Ok(Value::Object(self.realm_of(self.frames[fi].realm).global));
                }
                if &*n == "*new.target*" {
                    return Ok(Value::Undefined);
                }
                if or_undefined {
                    return Ok(Value::Undefined);
                }
                Err(self.not_defined(&n))
            }
        }
    }

    pub(crate) fn store_dynamic(
        &mut self,
        fi: usize,
        code: &Rc<CodeBlock>,
        name: u32,
        v: Value,
        strict: bool,
        init: bool,
    ) -> JsResult<()> {
        let n: Rc<str> = code.names[name as usize].clone();
        let env = self.frames[fi].env;
        match self.resolve_binding(env, &n)? {
            Binding::Slot { env, slot, kind } => {
                if !init {
                    let cur = self.heap.env(env).as_decl().unwrap().slots[slot];
                    if cur.is_empty() && kind != BindingKind::Var && kind != BindingKind::Function {
                        return Err(self.tdz_error(&n));
                    }
                    if kind.is_const() {
                        return Err(
                            self.throw_type(&format!("Assignment to constant variable '{n}'"))
                        );
                    }
                    if kind == BindingKind::Internal || kind == BindingKind::FuncName {
                        if strict {
                            return Err(
                                self.throw_type(&format!("Assignment to constant variable '{n}'"))
                            );
                        }
                        return Ok(());
                    }
                }
                self.heap.env_mut(env).as_decl_mut().unwrap().slots[slot] = v;
                Ok(())
            }
            Binding::Extra { env } => {
                let d = self.heap.env_mut(env).as_decl_mut().unwrap();
                let b = d.extra.as_mut().unwrap().get_mut(&*n).unwrap();
                if !init && b.kind.is_const() {
                    return Err(self.throw_type(&format!("Assignment to constant variable '{n}'")));
                }
                b.value = v;
                Ok(())
            }
            Binding::Object { obj, with } => {
                let key = self.key(&n);
                if with && strict && !self.has_property(obj, key)? {
                    return Err(self.not_defined(&n));
                }
                let ok = self.set_prop(obj, key, v, Value::Object(obj))?;
                if !ok && strict {
                    return Err(self.throw_type(&format!(
                        "Cannot assign to read only property '{n}' of object"
                    )));
                }
                Ok(())
            }
            Binding::GlobalLexical => {
                let realm = self.frames[fi].realm;
                let b = self.realms[realm.0 as usize]
                    .global_lexicals
                    .get_mut(&*n)
                    .unwrap();
                if !init {
                    if b.value.is_empty() {
                        return Err(self.tdz_error(&n));
                    }
                    if b.kind.is_const() {
                        return Err(
                            self.throw_type(&format!("Assignment to constant variable '{n}'"))
                        );
                    }
                }
                b.value = v;
                Ok(())
            }
            Binding::GlobalProperty => {
                let global = self.realm_of(self.frames[fi].realm).global;
                let key = self.key(&n);
                let ok = self.set_prop(global, key, v, Value::Object(global))?;
                if !ok && strict {
                    return Err(self.throw_type(&format!(
                        "Cannot assign to read only property '{n}' of object"
                    )));
                }
                Ok(())
            }
            Binding::Unresolvable => {
                if strict && !init {
                    return Err(self.not_defined(&n));
                }
                let global = self.realm_of(self.frames[fi].realm).global;
                let key = self.key(&n);
                let ok = self.set_prop(global, key, v, Value::Object(global))?;
                if !ok && strict {
                    return Err(self.throw_type(&format!(
                        "Cannot assign to read only property '{n}' of object"
                    )));
                }
                Ok(())
            }
        }
    }

    // ---- pre-resolved references ----------------------------------------------------
    //
    // A reference taken by `ResolveDynamic` lives in a register as:
    //   Object(o)  an object environment (`with`) whose object is `o`
    //   Number(e)  the declarative environment with arena index `e`
    //   Null       a global binding (lexical or property)
    //   Undefined  unresolvable at resolution time
    // Registers are never observable from JavaScript, and the environment
    // stays reachable through the frame while the right-hand side runs.

    pub(crate) fn resolve_dynamic_ref(
        &mut self,
        fi: usize,
        code: &Rc<CodeBlock>,
        name: u32,
    ) -> JsResult<Value> {
        let n: Rc<str> = code.names[name as usize].clone();
        let env = self.frames[fi].env;
        Ok(match self.resolve_binding(env, &n)? {
            Binding::Slot { env, .. } | Binding::Extra { env } => Value::Number(env.0 as f64),
            Binding::Object { obj, .. } => Value::Object(obj),
            Binding::GlobalLexical | Binding::GlobalProperty => Value::Null,
            Binding::Unresolvable => Value::Undefined,
        })
    }

    pub(crate) fn load_dynamic_ref(
        &mut self,
        fi: usize,
        code: &Rc<CodeBlock>,
        name: u32,
        reference: Value,
    ) -> JsResult<Value> {
        let n: Rc<str> = code.names[name as usize].clone();
        match reference {
            Value::Object(obj) => {
                let key = self.key(&n);
                if !self.has_property(obj, key)? {
                    if code.strict {
                        return Err(self.not_defined(&n));
                    }
                    return Ok(Value::Undefined);
                }
                self.get_prop(obj, key, Value::Object(obj))
            }
            Value::Number(e) => {
                let env = EnvId(e as u32);
                let d = self
                    .heap
                    .env(env)
                    .as_decl()
                    .expect("reference to a non-declarative environment");
                let v = match d.slot_of(&n) {
                    Some((slot, _)) => d.slots[slot],
                    None => match d.extra_get(&n) {
                        Some(b) => b.value,
                        None => return Err(self.not_defined(&n)),
                    },
                };
                if v.is_empty() {
                    return Err(self.tdz_error(&n));
                }
                Ok(v)
            }
            Value::Null => {
                let realm = self.frames[fi].realm;
                if let Some(b) = self.realms[realm.0 as usize].global_lexicals.get(&*n) {
                    if b.value.is_empty() {
                        return Err(self.tdz_error(&n));
                    }
                    return Ok(b.value);
                }
                let global = self.realm_of(realm).global;
                let key = self.key(&n);
                if !self.has_property(global, key)? {
                    return Err(self.not_defined(&n));
                }
                self.get_prop(global, key, Value::Object(global))
            }
            _ => Err(self.not_defined(&n)),
        }
    }

    pub(crate) fn store_dynamic_ref(
        &mut self,
        fi: usize,
        code: &Rc<CodeBlock>,
        name: u32,
        reference: Value,
        v: Value,
        strict: bool,
    ) -> JsResult<()> {
        let n: Rc<str> = code.names[name as usize].clone();
        match reference {
            Value::Object(obj) => {
                let key = self.key(&n);
                let still_exists = self.has_property(obj, key)?;
                if !still_exists && strict {
                    return Err(self.not_defined(&n));
                }
                let ok = self.set_prop(obj, key, v, Value::Object(obj))?;
                if !ok && strict {
                    return Err(self.throw_type(&format!(
                        "Cannot assign to read only property '{n}' of object"
                    )));
                }
                Ok(())
            }
            Value::Number(e) => {
                let env = EnvId(e as u32);
                let d = self
                    .heap
                    .env(env)
                    .as_decl()
                    .expect("reference to a non-declarative environment");
                if let Some((slot, kind)) = d.slot_of(&n) {
                    let cur = d.slots[slot];
                    if cur.is_empty() && kind != BindingKind::Var && kind != BindingKind::Function {
                        return Err(self.tdz_error(&n));
                    }
                    if kind.is_const() {
                        return Err(
                            self.throw_type(&format!("Assignment to constant variable '{n}'"))
                        );
                    }
                    if kind == BindingKind::Internal || kind == BindingKind::FuncName {
                        if strict {
                            return Err(
                                self.throw_type(&format!("Assignment to constant variable '{n}'"))
                            );
                        }
                        return Ok(());
                    }
                    self.heap.env_mut(env).as_decl_mut().unwrap().slots[slot] = v;
                    return Ok(());
                }
                match d.extra_get(&n) {
                    Some(b) => {
                        if b.kind.is_const() {
                            return Err(
                                self.throw_type(&format!("Assignment to constant variable '{n}'"))
                            );
                        }
                        let d = self.heap.env_mut(env).as_decl_mut().unwrap();
                        d.extra.as_mut().unwrap().get_mut(&*n).unwrap().value = v;
                        Ok(())
                    }
                    None => {
                        // The binding was deleted since the reference was
                        // taken: strict code throws, sloppy code recreates it.
                        if strict {
                            return Err(self.not_defined(&n));
                        }
                        let d = self.heap.env_mut(env).as_decl_mut().unwrap();
                        d.extra_insert(
                            n.clone(),
                            ExtraBinding {
                                value: v,
                                kind: BindingKind::Var,
                                deletable: true,
                            },
                        );
                        Ok(())
                    }
                }
            }
            Value::Null => {
                let realm = self.frames[fi].realm;
                if let Some(b) = self.realms[realm.0 as usize].global_lexicals.get_mut(&*n) {
                    if b.value.is_empty() {
                        return Err(self.tdz_error(&n));
                    }
                    if b.kind.is_const() {
                        return Err(
                            self.throw_type(&format!("Assignment to constant variable '{n}'"))
                        );
                    }
                    b.value = v;
                    return Ok(());
                }
                let global = self.realm_of(realm).global;
                let key = self.key(&n);
                if strict && !self.has_property(global, key)? {
                    // Deleted between ResolveBinding and PutValue.
                    return Err(self.not_defined(&n));
                }
                let ok = self.set_prop(global, key, v, Value::Object(global))?;
                if !ok && strict {
                    return Err(self.throw_type(&format!(
                        "Cannot assign to read only property '{n}' of object"
                    )));
                }
                Ok(())
            }
            _ => {
                if strict {
                    return Err(self.not_defined(&n));
                }
                let global = self.realm_of(self.frames[fi].realm).global;
                let key = self.key(&n);
                self.set_prop(global, key, v, Value::Object(global))?;
                Ok(())
            }
        }
    }

    pub(crate) fn delete_dynamic(
        &mut self,
        fi: usize,
        code: &Rc<CodeBlock>,
        name: u32,
    ) -> JsResult<bool> {
        let n: Rc<str> = code.names[name as usize].clone();
        let env = self.frames[fi].env;
        match self.resolve_binding(env, &n)? {
            Binding::Slot { .. } | Binding::GlobalLexical => Ok(false),
            Binding::Extra { env } => {
                let d = self.heap.env_mut(env).as_decl_mut().unwrap();
                let ex = d.extra.as_mut().unwrap();
                if ex.get(&*n).map(|b| b.deletable).unwrap_or(false) {
                    ex.shift_remove(&*n);
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            Binding::Object { obj, .. } => {
                let key = self.key(&n);
                self.delete_property(obj, key)
            }
            Binding::GlobalProperty => {
                let global = self.realm_of(self.frames[fi].realm).global;
                let key = self.key(&n);
                let r = self.delete_property(global, key)?;
                if r {
                    self.realms[self.frames[fi].realm.0 as usize]
                        .var_names
                        .remove(&*n);
                }
                Ok(r)
            }
            Binding::Unresolvable => Ok(true),
        }
    }

    pub(crate) fn this_for_dynamic_call(
        &mut self,
        fi: usize,
        code: &Rc<CodeBlock>,
        name: u32,
    ) -> JsResult<Value> {
        let n: Rc<str> = code.names[name as usize].clone();
        let env = self.frames[fi].env;
        match self.resolve_binding(env, &n)? {
            Binding::Object { obj, with: true } => Ok(Value::Object(obj)),
            _ => Ok(Value::Undefined),
        }
    }

    // ---- declaration instantiation --------------------------------------------------

    pub(crate) fn declare_globals(&mut self, fi: usize, code: &Rc<CodeBlock>) -> JsResult<()> {
        match code.kind {
            CodeKind::Script => self.global_declaration_instantiation(fi, code),
            CodeKind::Eval => self.eval_declaration_instantiation(fi, code),
            _ => Ok(()),
        }
    }

    fn already_declared(&mut self, n: &str) -> Fault {
        self.throw_syntax(&format!("Identifier '{n}' has already been declared"))
    }

    /// GlobalDeclarationInstantiation.
    fn global_declaration_instantiation(
        &mut self,
        fi: usize,
        code: &Rc<CodeBlock>,
    ) -> JsResult<()> {
        let Some(decls) = &code.declarations else {
            return Ok(());
        };
        let realm = self.frames[fi].realm;
        let global = self.realm_of(realm).global;
        // Step 3-4: lexical names may not clash with existing declarations.
        for (n, _) in &decls.lexical {
            let r = &self.realms[realm.0 as usize];
            if r.var_names.contains(&**n) || r.global_lexicals.contains_key(&**n) {
                return Err(self.already_declared(n));
            }
            let key = self.key(n);
            if let Some(p) = self.heap.get_own_property(global, key) {
                if !p.attrs.configurable() {
                    return Err(self.already_declared(n));
                }
            }
        }
        let var_like: Vec<Rc<str>> = decls
            .var_names
            .iter()
            .cloned()
            .chain(decls.functions.iter().map(|(n, _)| n.clone()))
            .collect();
        for n in &var_like {
            if self.realms[realm.0 as usize]
                .global_lexicals
                .contains_key(&**n)
            {
                return Err(self.already_declared(n));
            }
        }
        // Functions to instantiate: last declaration of a name wins.
        let mut functions: Vec<(Rc<str>, u32)> = Vec::new();
        for (n, idx) in decls.functions.iter().rev() {
            if !functions.iter().any(|(m, _)| m == n) {
                functions.push((n.clone(), *idx));
            }
        }
        functions.reverse();
        for (n, _) in &functions {
            if !self.can_declare_global_function(global, n) {
                return Err(self.throw_type(&format!("Cannot declare global function '{n}'")));
            }
        }
        let fn_names: Vec<Rc<str>> = functions.iter().map(|(n, _)| n.clone()).collect();
        let mut declared_vars: Vec<Rc<str>> = Vec::new();
        for n in &decls.var_names {
            if fn_names.contains(n) || declared_vars.contains(n) {
                continue;
            }
            if !self.can_declare_global_var(global, n) {
                return Err(self.throw_type(&format!("Cannot declare global variable '{n}'")));
            }
            declared_vars.push(n.clone());
        }
        // Annex B.3.3.2: block-level functions hoisted as vars.
        for n in &decls.annex_b_functions {
            if fn_names.contains(n) || declared_vars.contains(n) {
                continue;
            }
            if self.realms[realm.0 as usize]
                .global_lexicals
                .contains_key(&**n)
            {
                continue;
            }
            if self.can_declare_global_function(global, n) {
                self.create_global_var_binding(realm, global, n, false);
                declared_vars.push(n.clone());
            }
        }
        // Lexical bindings start in the TDZ. A new lexical may shadow a
        // global property that some site has cached.
        if !decls.lexical.is_empty() {
            let r = &mut self.realms[realm.0 as usize];
            r.epoch = r.epoch.wrapping_add(1);
            for (n, kind) in &decls.lexical {
                r.global_lexicals.insert(
                    n.clone(),
                    LexBinding {
                        value: Value::Empty,
                        kind: *kind,
                    },
                );
            }
        }
        let env = self.frames[fi].env;
        let module = self.frames[fi].module;
        for (n, idx) in &functions {
            let fo = self.create_function(code.functions[*idx as usize].clone(), env, module);
            self.create_global_function_binding(realm, global, n, Value::Object(fo), false)?;
        }
        for n in &declared_vars {
            self.create_global_var_binding(realm, global, n, false);
        }
        Ok(())
    }

    fn can_declare_global_function(&mut self, global: ObjId, n: &str) -> bool {
        let key = self.key(n);
        match self.heap.get_own_property(global, key) {
            None => self.heap.obj(global).extensible,
            Some(p) => {
                p.attrs.configurable()
                    || (!p.is_accessor() && p.attrs.writable() && p.attrs.enumerable())
            }
        }
    }

    fn can_declare_global_var(&mut self, global: ObjId, n: &str) -> bool {
        let key = self.key(n);
        self.heap.has_own_property(global, key) || self.heap.obj(global).extensible
    }

    fn create_global_var_binding(
        &mut self,
        realm: agnijs_engine_api::RealmId,
        global: ObjId,
        n: &Rc<str>,
        deletable: bool,
    ) {
        let key = self.key(n);
        if !self.heap.has_own_property(global, key) && self.heap.obj(global).extensible {
            let attrs = Attrs::new(true, true, deletable);
            self.heap
                .add_own_property(global, key, Property::data(Value::Undefined, attrs));
        }
        // Bindings introduced by non-strict direct eval are configurable and
        // are not var declarations for later scripts' lexical checks.
        if !deletable {
            self.realms[realm.0 as usize].var_names.insert(n.clone());
        }
    }

    fn create_global_function_binding(
        &mut self,
        realm: agnijs_engine_api::RealmId,
        global: ObjId,
        n: &Rc<str>,
        v: Value,
        deletable: bool,
    ) -> JsResult<()> {
        let key = self.key(n);
        let existing = self.heap.get_own_property(global, key);
        let desc = match existing {
            Some(p) if !p.attrs.configurable() => crate::value::PropertyDescriptor {
                value: Some(v),
                ..Default::default()
            },
            _ => crate::value::PropertyDescriptor {
                value: Some(v),
                writable: Some(true),
                enumerable: Some(true),
                configurable: Some(deletable),
                ..Default::default()
            },
        };
        self.define_property_or_throw(global, key, desc)?;
        self.set_prop(global, key, v, Value::Object(global))?;
        if !deletable {
            self.realms[realm.0 as usize].var_names.insert(n.clone());
        }
        Ok(())
    }

    /// EvalDeclarationInstantiation.
    /// The variable environment of sloppy eval code whose outer environment
    /// is `outer`: the nearest function var scope (or parameter scope), or
    /// `None` for the global object environment.
    pub(crate) fn eval_var_env(&self, outer: Option<EnvId>) -> Option<EnvId> {
        let mut cur = outer;
        while let Some(e) = cur {
            match self.heap.env(e) {
                Environment::Declarative(d) => {
                    if d.is_var_scope {
                        return Some(e);
                    }
                    // A parameter scope with a separate body scope: an eval
                    // in a parameter expression declares its vars here.
                    let is_param_scope = matches!(
                        d.scope.as_ref().map(|(c, i)| c.scopes[*i as usize].kind),
                        Some(ScopeKind::Function)
                    );
                    if is_param_scope {
                        return Some(e);
                    }
                    cur = d.parent;
                }
                Environment::Object(o) => cur = o.parent,
                Environment::Global(_) => return None,
            }
        }
        None
    }

    /// Annex B.3.3.3 function evaluation in sloppy eval code:
    /// SetMutableBinding on the eval's variable environment.
    pub(crate) fn store_eval_var(
        &mut self,
        fi: usize,
        code: &Rc<CodeBlock>,
        name: u32,
        v: Value,
    ) -> JsResult<()> {
        let n: Rc<str> = code.names[name as usize].clone();
        let frame_env = self.frames[fi].base_env;
        let own_env = frame_env.filter(|e| matches!(self.heap.env(*e).scope_info(), Some(info) if info.kind == ScopeKind::Eval));
        let outer = match own_env {
            Some(e) => self.heap.env(e).parent(),
            None => frame_env,
        };
        match self.eval_var_env(outer) {
            Some(e) => {
                let d = self.heap.env_mut(e).as_decl_mut().unwrap();
                if let Some((slot, _)) = d.slot_of(&n) {
                    d.slots[slot] = v;
                } else if let Some(b) = d.extra.as_mut().and_then(|x| x.get_mut(&*n)) {
                    b.value = v;
                } else {
                    d.extra_insert(
                        n.clone(),
                        ExtraBinding {
                            value: v,
                            kind: BindingKind::Var,
                            deletable: true,
                        },
                    );
                }
                Ok(())
            }
            None => {
                let realm = self.frames[fi].realm;
                let global = self.realm_of(realm).global;
                let key = self.key(&n);
                self.set_prop(global, key, v, Value::Object(global))?;
                Ok(())
            }
        }
    }

    fn eval_declaration_instantiation(&mut self, fi: usize, code: &Rc<CodeBlock>) -> JsResult<()> {
        let Some(decls) = &code.declarations else {
            return Ok(());
        };
        let realm = self.frames[fi].realm;
        let frame_env = self.frames[fi].env;
        let module = self.frames[fi].module;
        // The eval's own environment (if the compiler pushed one).
        let own_env = frame_env.filter(|e| matches!(self.heap.env(*e).scope_info(), Some(info) if info.kind == ScopeKind::Eval));
        let outer = match own_env {
            Some(e) => self.heap.env(e).parent(),
            None => frame_env,
        };
        let mut functions: Vec<(Rc<str>, u32)> = Vec::new();
        for (n, idx) in decls.functions.iter().rev() {
            if !functions.iter().any(|(m, _)| m == n) {
                functions.push((n.clone(), *idx));
            }
        }
        functions.reverse();

        if code.strict {
            // Vars and functions are slots of the eval environment.
            let Some(env) = own_env else { return Ok(()) };
            for (n, idx) in &functions {
                let fo =
                    self.create_function(code.functions[*idx as usize].clone(), Some(env), module);
                if let Some((slot, _)) = self.heap.env(env).as_decl().unwrap().slot_of(n) {
                    self.heap.env_mut(env).as_decl_mut().unwrap().slots[slot] = Value::Object(fo);
                }
            }
            return Ok(());
        }

        // Sloppy: find the variable environment.
        let var_env = self.eval_var_env(outer);
        let var_like: Vec<Rc<str>> = decls
            .var_names
            .iter()
            .cloned()
            .chain(functions.iter().map(|(n, _)| n.clone()))
            .collect();
        let global = self.realm_of(realm).global;
        if var_env.is_none() {
            for n in &var_like {
                if self.realms[realm.0 as usize]
                    .global_lexicals
                    .contains_key(&**n)
                {
                    return Err(self.already_declared(n));
                }
            }
        }
        // Lexical conflicts between the eval and its variable environment.
        let mut cur = outer;
        while let Some(e) = cur {
            if Some(e) == var_env {
                break;
            }
            match self.heap.env(e) {
                Environment::Declarative(d) => {
                    let is_catch = matches!(
                        d.scope.as_ref().map(|(c, i)| c.scopes[*i as usize].kind),
                        Some(ScopeKind::Catch)
                    );
                    for n in &var_like {
                        let found = d.slot_of(n).is_some() || d.extra_get(n).is_some();
                        if found && !is_catch {
                            return Err(self.already_declared(n));
                        }
                    }
                    cur = d.parent;
                }
                Environment::Object(o) => cur = o.parent,
                Environment::Global(_) => break,
            }
        }
        // Conflicts in the variable scope itself: its top-level lexical
        // declarations (which the spec keeps in a separate record) and, for
        // a parameter scope, every slot binding.
        if let Some(e) = var_env {
            let d = self.heap.env(e).as_decl().unwrap();
            let is_param_scope = !d.is_var_scope;
            for n in &var_like {
                if let Some((_, kind)) = d.slot_of(n) {
                    if is_param_scope || kind.is_lexical() {
                        return Err(self.already_declared(n));
                    }
                }
            }
        }
        if var_env.is_none() {
            for (n, _) in &functions {
                if !self.can_declare_global_function(global, n) {
                    return Err(self.throw_type(&format!("Cannot declare global function '{n}'")));
                }
            }
        }
        let fn_names: Vec<Rc<str>> = functions.iter().map(|(n, _)| n.clone()).collect();
        let mut declared_vars: Vec<Rc<str>> = Vec::new();
        for n in &decls.var_names {
            if fn_names.contains(n) || declared_vars.contains(n) {
                continue;
            }
            if var_env.is_none() && !self.can_declare_global_var(global, n) {
                return Err(self.throw_type(&format!("Cannot declare global variable '{n}'")));
            }
            declared_vars.push(n.clone());
        }
        // Annex B.3.3.3
        for n in &decls.annex_b_functions {
            if fn_names.contains(n) || declared_vars.contains(n) {
                continue;
            }
            // Skip when an intermediate scope binds the name.
            let mut blocked = false;
            let mut cur = outer;
            while let Some(e) = cur {
                if Some(e) == var_env {
                    break;
                }
                match self.heap.env(e) {
                    Environment::Declarative(d) => {
                        if d.slot_of(n).is_some() || d.extra_get(n).is_some() {
                            blocked = true;
                            break;
                        }
                        cur = d.parent;
                    }
                    Environment::Object(o) => cur = o.parent,
                    Environment::Global(_) => break,
                }
            }
            if blocked {
                continue;
            }
            if var_env.is_none()
                && self.realms[realm.0 as usize]
                    .global_lexicals
                    .contains_key(&**n)
            {
                continue;
            }
            match var_env {
                None => {
                    if self.can_declare_global_var(global, n) {
                        self.create_global_var_binding(realm, global, n, true);
                        declared_vars.push(n.clone());
                    }
                }
                Some(e) => {
                    let d = self.heap.env_mut(e).as_decl_mut().unwrap();
                    if d.slot_of(n).is_none() && d.extra_get(n).is_none() {
                        d.extra_insert(
                            n.clone(),
                            ExtraBinding {
                                value: Value::Undefined,
                                kind: BindingKind::Var,
                                deletable: true,
                            },
                        );
                    }
                    declared_vars.push(n.clone());
                }
            }
        }
        for (n, idx) in &functions {
            let fo = self.create_function(code.functions[*idx as usize].clone(), frame_env, module);
            match var_env {
                None => {
                    self.create_global_function_binding(realm, global, n, Value::Object(fo), true)?
                }
                Some(e) => {
                    let d = self.heap.env_mut(e).as_decl_mut().unwrap();
                    if let Some((slot, _)) = d.slot_of(n) {
                        d.slots[slot] = Value::Object(fo);
                    } else if let Some(b) = d.extra.as_mut().and_then(|x| x.get_mut(&**n)) {
                        b.value = Value::Object(fo);
                    } else {
                        d.extra_insert(
                            n.clone(),
                            ExtraBinding {
                                value: Value::Object(fo),
                                kind: BindingKind::Var,
                                deletable: true,
                            },
                        );
                    }
                }
            }
        }
        for n in &declared_vars {
            match var_env {
                None => self.create_global_var_binding(realm, global, n, true),
                Some(e) => {
                    let d = self.heap.env_mut(e).as_decl_mut().unwrap();
                    if d.slot_of(n).is_none() && d.extra_get(n).is_none() {
                        d.extra_insert(
                            n.clone(),
                            ExtraBinding {
                                value: Value::Undefined,
                                kind: BindingKind::Var,
                                deletable: true,
                            },
                        );
                    }
                }
            }
        }
        Ok(())
    }

    // ---- eval ---------------------------------------------------------------------

    /// Compile eval code from a string. A two-byte string goes over as its
    /// code units, so lone surrogates in it reach the lexer (TASK-100.15);
    /// a one-byte string is Latin-1 and cannot hold any.
    fn compile_eval_string(
        &mut self,
        s: crate::value::StrId,
        options: &EvalOptions,
    ) -> agnijs_js_compiler::Result<Rc<CodeBlock>> {
        match self.heap.units(s) {
            crate::string::Units::Two(u) => {
                agnijs_js_compiler::compile_eval_utf16(u, "eval", options)
            }
            one => {
                let text = one.to_string_lossy();
                agnijs_js_compiler::compile_eval(&text, "eval", options)
            }
        }
    }

    /// PerformEval for a direct call from frame `fi`.
    pub(crate) fn direct_eval(&mut self, fi: usize, arg: Value, strict: bool) -> JsResult<Value> {
        let Value::String(s) = arg else {
            return Ok(arg);
        };
        let realm = self.frames[fi].realm;
        if !self.host.can_compile_strings(realm) {
            return Err(self.throw(
                crate::vm::ErrorType::EvalError,
                "Code generation from strings disallowed for this context",
            ));
        }
        let options = self.eval_options(fi, strict);
        let code = match self.compile_eval_string(s, &options) {
            Ok(c) => c,
            Err(e) => return Err(self.throw_syntax(&e.message)),
        };
        self.debug_script_compiled(&code, agnijs_js_debugger::ScriptKind::Eval);
        let env = self.frames[fi].env;
        let realm = self.frames[fi].realm;
        let this = self.frames[fi].this;
        let callee = self.frames[fi].callee;
        let new_target = self.frames[fi].new_target;
        let module = self.frames[fi].module;
        self.run_eval_code(code, env, realm, this, callee, new_target, module)
    }

    /// Indirect eval: global environment of `realm`.
    pub fn indirect_eval(
        &mut self,
        realm: agnijs_engine_api::RealmId,
        arg: Value,
    ) -> JsResult<Value> {
        let Value::String(s) = arg else {
            return Ok(arg);
        };
        if !self.host.can_compile_strings(realm) {
            return Err(self.throw(
                crate::vm::ErrorType::EvalError,
                "Code generation from strings disallowed for this context",
            ));
        }
        let options = EvalOptions::default();
        let code = match self.compile_eval_string(s, &options) {
            Ok(c) => c,
            Err(e) => return Err(self.throw_syntax(&e.message)),
        };
        self.debug_script_compiled(&code, agnijs_js_debugger::ScriptKind::Eval);
        let env = self.realm_of(realm).global_env;
        let global = self.realm_of(realm).global;
        self.run_eval_code(
            code,
            Some(env),
            realm,
            Value::Object(global),
            None,
            Value::Undefined,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn run_eval_code(
        &mut self,
        code: Rc<CodeBlock>,
        env: Option<EnvId>,
        realm: agnijs_engine_api::RealmId,
        this: Value,
        callee: Option<ObjId>,
        new_target: Value,
        module: Option<ObjId>,
    ) -> JsResult<Value> {
        let frame = Frame {
            code,
            pc: 0,
            base: 0,
            acc: Value::Undefined,
            env,
            base_env: env,
            env_depth: 0,
            this,
            callee,
            new_target,
            args: Vec::new(),
            argc: 0,
            realm,
            exit: true,
            discard_result: false,
            construct: ConstructKind::None,
            kind: FrameKind::Eval,
            generator: None,
            module,
        };
        match self.run_entry(true, |vm| vm.push_frame(frame))? {
            RunOutcome::Return(v) => Ok(v),
            other => panic!("eval run ended with {other:?}"),
        }
    }

    /// The parser context of a direct eval at frame `fi`.
    pub(crate) fn eval_options(&mut self, fi: usize, strict: bool) -> EvalOptions {
        let code = self.frames[fi].code.clone();
        let env = self.frames[fi].env;
        let mut opts = EvalOptions {
            strict,
            ..Default::default()
        };
        // Collect private names and pseudo-bindings from the scope chain.
        let mut cur = env;
        let mut saw_new_target = false;
        let mut func_binding: Option<Value> = None;
        while let Some(e) = cur {
            if let Environment::Declarative(d) = self.heap.env(e) {
                if let Some(info) = d.scope.as_ref().map(|(c, i)| &c.scopes[*i as usize]) {
                    for (i, (n, _)) in info.bindings.iter().enumerate() {
                        if let Some(bare) = n.strip_prefix('#') {
                            opts.private_names.push(Rc::from(bare));
                        } else if &**n == "*new.target*" {
                            saw_new_target = true;
                        } else if &**n == "*func*" && func_binding.is_none() {
                            func_binding = Some(d.slots[i]);
                        }
                    }
                }
            }
            cur = self.heap.env(e).parent();
        }
        let in_function_kind = |k: CodeKind| {
            matches!(
                k,
                CodeKind::Function
                    | CodeKind::Method
                    | CodeKind::ClassConstructor
                    | CodeKind::ClassInitializer
            )
        };
        if in_function_kind(code.kind) {
            opts.in_function = true;
        } else if code.kind == CodeKind::Arrow
            && (saw_new_target || self.frames[fi].callee.is_some())
        {
            // An arrow inside a function: the enclosing function is
            // poisoned and binds `*new.target*`.
            opts.in_function = saw_new_target;
        }
        // Method-ness: the function (or the arrow's enclosing function) has a home object.
        let callee = self.frames[fi].callee;
        let func_obj = if code.kind == CodeKind::Arrow {
            func_binding.and_then(|v| v.as_object())
        } else {
            callee
        };
        if let Some(fo) = func_obj {
            if let Some(fd) = self.heap.obj(fo).function() {
                if fd.home_object.is_some() {
                    opts.in_method = true;
                }
                if fd.code.is_derived_constructor {
                    opts.in_derived_constructor = true;
                }
                if fd.code.kind == CodeKind::ClassInitializer {
                    opts.in_class_field_initializer = true;
                }
            }
        }
        if code.kind == CodeKind::ClassInitializer {
            opts.in_class_field_initializer = true;
        }
        if code.kind == CodeKind::Arrow && opts.in_class_field_initializer {
            opts.in_function = true;
        }
        opts
    }
}
