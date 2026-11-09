//! Module records, the loader (through the host's hooks), linking,
//! evaluation with top-level await, and namespace exotic objects.
//!
//! The compiler leaves module code's top-level environment to the host
//! (`env_by_host`): the loader creates it from the code block's module
//! scope, instantiates function declarations and namespace imports into it,
//! and runs the body as a function closed over that environment. Named
//! imports are indirections resolved at link time (`import_bindings`) and
//! read by `LdaImport`.

use crate::builtins::promise::{self, Capability};
use crate::gc::Tracer;
use crate::object::{CallArgs, Class, ObjectData};
use crate::value::{Attrs, EnvId, ObjId, PropKey, Property, PropertyDescriptor, StrId, Value};
use crate::vm::{ErrorType, Fault, JsResult, Vm};
use agnijs_engine_api::{ImportAttribute, ModuleSource, RealmId};
use agnijs_js_compiler::bytecode::{CodeBlock, ScopeKind};
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModuleStatus {
    Unlinked,
    Linking,
    Linked,
    Evaluating,
    EvaluatingAsync,
    Evaluated,
}

/// Where a named import or export resolves to.
#[derive(Clone, Copy, Debug)]
pub enum Binding {
    /// A slot of `module`'s environment.
    Slot { module: ObjId, slot: usize },
    /// The namespace object of `module` (`export * as ns from`).
    Namespace(ObjId),
}

#[derive(Clone, Copy, Debug)]
enum Resolution {
    Found(Binding),
    NotFound,
    Ambiguous,
}

pub struct ModuleRecord {
    pub specifier: String,
    pub realm: RealmId,
    /// `None` for synthetic (JSON) modules.
    pub code: Option<Rc<CodeBlock>>,
    pub env: Option<EnvId>,
    pub function: Option<ObjId>,
    pub namespace: Option<ObjId>,
    pub status: ModuleStatus,
    pub error: Option<Value>,
    /// Resolved records, parallel to `ModuleInfo::requested`.
    pub requested: Vec<Option<ObjId>>,
    /// Resolved named imports, parallel to `ModuleInfo::imports`.
    pub import_bindings: Vec<Option<Binding>>,
    /// The default export of a synthetic module.
    pub synthetic_value: Option<Value>,
    pub import_meta: Option<ObjId>,
    pub dfs_index: usize,
    pub dfs_ancestor_index: usize,
    pub cycle_root: Option<ObjId>,
    pub has_tla: bool,
    pub async_evaluation: bool,
    pub async_order: u64,
    pub top_level_capability: Option<Capability>,
    pub async_parents: Vec<ObjId>,
    pub pending_async_deps: usize,
}

impl ModuleRecord {
    pub fn trace(&self, t: &mut Tracer) {
        t.opt_env(self.env);
        t.opt_obj(self.function);
        t.opt_obj(self.namespace);
        if let Some(e) = self.error {
            t.value(e);
        }
        for r in self.requested.iter().flatten() {
            t.obj(*r);
        }
        for b in self.import_bindings.iter().flatten() {
            match b {
                Binding::Slot { module, .. } | Binding::Namespace(module) => t.obj(*module),
            }
        }
        if let Some(v) = self.synthetic_value {
            t.value(v);
        }
        t.opt_obj(self.import_meta);
        t.opt_obj(self.cycle_root);
        if let Some(c) = &self.top_level_capability {
            c.trace(t);
        }
        for p in &self.async_parents {
            t.obj(*p);
        }
    }
}

pub struct NamespaceData {
    pub module: ObjId,
    /// Sorted export names.
    pub exports: Vec<StrId>,
}

fn record(vm: &Vm, m: ObjId) -> &ModuleRecord {
    match &vm.heap.obj(m).class {
        Class::Module(r) => r,
        _ => panic!("not a module record"),
    }
}

fn record_mut(vm: &mut Vm, m: ObjId) -> &mut ModuleRecord {
    match &mut vm.heap.obj_mut(m).class {
        Class::Module(r) => r,
        _ => panic!("not a module record"),
    }
}

fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

impl Vm {
    /// A module's export/import name as a Rust string.
    fn module_specifier_string(&self, s: &[u16]) -> String {
        String::from_utf16_lossy(s)
    }

    // ---- loading --------------------------------------------------------------------------

    /// Load a module graph rooted at `specifier`, link it and start its
    /// evaluation. Returns the evaluation promise.
    pub fn load_and_evaluate_module(
        &mut self,
        realm: RealmId,
        referrer: Option<&str>,
        specifier: &str,
        attributes: &[ImportAttribute],
    ) -> JsResult<ObjId> {
        let saved = self.current_realm;
        self.current_realm = realm;
        let r = self.load_and_evaluate_inner(realm, referrer, specifier, attributes);
        self.current_realm = saved;
        r
    }

    fn load_and_evaluate_inner(
        &mut self,
        realm: RealmId,
        referrer: Option<&str>,
        specifier: &str,
        attributes: &[ImportAttribute],
    ) -> JsResult<ObjId> {
        let module = match self.load_module_graph(realm, referrer, specifier, attributes) {
            Ok(m) => m,
            Err(Fault::Throw(e)) => return self.rejected_promise(e),
            Err(f) => return Err(f),
        };
        if let Err(f) = self.link_module(module) {
            match f {
                Fault::Throw(e) => return self.rejected_promise(e),
                other => return Err(other),
            }
        }
        let evaluated = self.evaluate_module(module)?;
        // The host's promise settles with the namespace object.
        let cap = promise::new_promise_capability_intrinsic(self);
        let on_ok =
            self.new_native_closure("", 1, resolve_with_namespace, vec![Value::Object(module)]);
        promise::perform_then(
            self,
            evaluated,
            Value::Object(on_ok),
            Value::Undefined,
            Some(cap.clone()),
        )?;
        Ok(cap.promise)
    }

    fn rejected_promise(&mut self, e: Value) -> JsResult<ObjId> {
        let cap = promise::new_promise_capability_intrinsic(self);
        promise::reject_capability(self, &cap, e)?;
        Ok(cap.promise)
    }

    /// Fetch (or find) a module record and, recursively, everything it
    /// requests. Errors are thrown values.
    fn load_module_graph(
        &mut self,
        realm: RealmId,
        referrer: Option<&str>,
        specifier: &str,
        attributes: &[ImportAttribute],
    ) -> JsResult<ObjId> {
        for a in attributes {
            if a.key != "type" {
                return self
                    .syntax_error(&format!("Import attribute '{}' is not supported", a.key));
            }
        }
        let host = self.host.clone();
        let resolved = match host.resolve_module(referrer, specifier, attributes) {
            Ok(r) => r,
            Err(msg) => return self.type_error(&msg),
        };
        if let Some(m) = self.modules.get(&(realm, resolved.clone())) {
            return Ok(*m);
        }
        let source = match host.load_module(&resolved) {
            Ok(s) => s,
            Err(msg) => return self.type_error(&msg),
        };
        let wants_json = attributes
            .iter()
            .any(|a| a.key == "type" && a.value == "json");
        let module = match source {
            ModuleSource::Source(text) => {
                if wants_json {
                    return self.type_error(&format!("Module '{resolved}' is not a JSON module"));
                }
                let code = match agnijs_js_compiler::compile_module(&text, &resolved) {
                    Ok(c) => c,
                    Err(e) => return self.syntax_error(&e.message),
                };
                self.note_script(&code, agnijs_js_debugger::ScriptKind::Module, realm);
                self.new_module_record(realm, &resolved, Some(code), None)
            }
            ModuleSource::Bytecode(bytes) => {
                let code = match crate::engine::decode_bytecode(&bytes, true) {
                    Ok(crate::engine::Decoded::Module(c)) => c,
                    Ok(_) => {
                        return self
                            .type_error(&format!("Bytecode for '{resolved}' is not a module"))
                    }
                    Err(e) => return self.type_error(&format!("{e:?}")),
                };
                if let Err(e) = self.verify_debug_sections(&code) {
                    return self.type_error(&format!("{e:?}"));
                }
                self.new_module_record(realm, &resolved, Some(code), None)
            }
            ModuleSource::Json(text) => {
                let v = crate::builtins::json::parse_json_text(self, &text)?;
                self.new_module_record(realm, &resolved, None, Some(v))
            }
        };
        self.modules.insert((realm, resolved.clone()), module);
        // Load what the module requests.
        let code = record(self, module).code.clone();
        if let Some(code) = code {
            let info = code
                .module
                .clone()
                .expect("module code without module info");
            for (i, req) in info.requested.iter().enumerate() {
                let spec = self.module_specifier_string(&req.specifier);
                let attrs: Vec<ImportAttribute> = req
                    .attributes
                    .iter()
                    .map(|(k, v)| ImportAttribute {
                        key: String::from_utf16_lossy(k),
                        value: String::from_utf16_lossy(v),
                    })
                    .collect();
                let dep = self.load_module_graph(realm, Some(&resolved), &spec, &attrs)?;
                record_mut(self, module).requested[i] = Some(dep);
            }
        }
        Ok(module)
    }

    fn new_module_record(
        &mut self,
        realm: RealmId,
        specifier: &str,
        code: Option<Rc<CodeBlock>>,
        synthetic: Option<Value>,
    ) -> ObjId {
        let (n_req, n_imp, has_tla) = match &code {
            Some(c) => {
                let info = c.module.as_ref().expect("module code without module info");
                (info.requested.len(), info.imports.len(), info.has_tla)
            }
            None => (0, 0, false),
        };
        let rec = ModuleRecord {
            specifier: specifier.to_string(),
            realm,
            code,
            env: None,
            function: None,
            namespace: None,
            status: ModuleStatus::Unlinked,
            error: None,
            requested: vec![None; n_req],
            import_bindings: vec![None; n_imp],
            synthetic_value: synthetic,
            import_meta: None,
            dfs_index: 0,
            dfs_ancestor_index: 0,
            cycle_root: None,
            has_tla,
            async_evaluation: false,
            async_order: 0,
            top_level_capability: None,
            async_parents: Vec::new(),
            pending_async_deps: 0,
        };
        self.alloc(ObjectData::new(None, Class::Module(Box::new(rec))))
    }

    // ---- export resolution -------------------------------------------------------------------

    /// GetExportedNames(module, exportStarSet).
    fn exported_names(&mut self, module: ObjId, star_set: &mut Vec<ObjId>) -> Vec<Vec<u16>> {
        if star_set.contains(&module) {
            return Vec::new();
        }
        star_set.push(module);
        let Some(code) = record(self, module).code.clone() else {
            return vec![utf16("default")];
        };
        let info = code.module.clone().unwrap();
        let mut names: Vec<Vec<u16>> = Vec::new();
        for e in info
            .local_exports
            .iter()
            .chain(info.indirect_exports.iter())
        {
            names.push(e.export_name.as_ref().unwrap().to_vec());
        }
        for &ri in &info.star_exports {
            let Some(req) = record(self, module).requested[ri] else {
                continue;
            };
            for n in self.exported_names(req, star_set) {
                if n.as_slice() != utf16("default").as_slice() && !names.contains(&n) {
                    names.push(n);
                }
            }
        }
        names
    }

    /// ResolveExport(module, exportName, resolveSet).
    fn resolve_export(
        &mut self,
        module: ObjId,
        name: &[u16],
        resolve_set: &mut Vec<(ObjId, Vec<u16>)>,
    ) -> Resolution {
        if resolve_set
            .iter()
            .any(|(m, n)| *m == module && n.as_slice() == name)
        {
            return Resolution::NotFound; // circular import request
        }
        resolve_set.push((module, name.to_vec()));
        let Some(code) = record(self, module).code.clone() else {
            // Synthetic module: only `default`.
            if name == utf16("default").as_slice() {
                return Resolution::Found(Binding::Slot { module, slot: 0 });
            }
            return Resolution::NotFound;
        };
        let info = code.module.clone().unwrap();
        for e in &info.local_exports {
            if e.export_name.as_deref() == Some(name) {
                let local = e.local_name.as_ref().unwrap();
                // `import ... ; export { imported }` re-exports the
                // imported binding (or namespace) itself.
                if let Some(imp) = info.imports.iter().find(|i| i.local_name == *local) {
                    let Some(req) = record(self, module).requested[imp.request] else {
                        return Resolution::NotFound;
                    };
                    return match &imp.import_name {
                        None => Resolution::Found(Binding::Namespace(req)),
                        Some(import_name) => self.resolve_export(req, import_name, resolve_set),
                    };
                }
                let slot = self
                    .module_slot_of(&code, local)
                    .expect("local export without a slot");
                return Resolution::Found(Binding::Slot { module, slot });
            }
        }
        for e in &info.indirect_exports {
            if e.export_name.as_deref() == Some(name) {
                let Some(req) = record(self, module).requested[e.request.unwrap()] else {
                    return Resolution::NotFound;
                };
                return match &e.import_name {
                    None => Resolution::Found(Binding::Namespace(req)),
                    Some(import_name) => self.resolve_export(req, import_name, resolve_set),
                };
            }
        }
        if name == utf16("default").as_slice() {
            return Resolution::NotFound;
        }
        let mut star_resolution: Option<Binding> = None;
        for &ri in &info.star_exports {
            let Some(req) = record(self, module).requested[ri] else {
                continue;
            };
            match self.resolve_export(req, name, resolve_set) {
                Resolution::Ambiguous => return Resolution::Ambiguous,
                Resolution::NotFound => {}
                Resolution::Found(b) => match star_resolution {
                    None => star_resolution = Some(b),
                    Some(prev) => {
                        let same = match (prev, b) {
                            (
                                Binding::Slot {
                                    module: m1,
                                    slot: s1,
                                },
                                Binding::Slot {
                                    module: m2,
                                    slot: s2,
                                },
                            ) => m1 == m2 && s1 == s2,
                            (Binding::Namespace(m1), Binding::Namespace(m2)) => m1 == m2,
                            _ => false,
                        };
                        if !same {
                            return Resolution::Ambiguous;
                        }
                    }
                },
            }
        }
        match star_resolution {
            Some(b) => Resolution::Found(b),
            None => Resolution::NotFound,
        }
    }

    fn module_scope_index(code: &CodeBlock) -> u32 {
        code.scopes
            .iter()
            .position(|s| s.kind == ScopeKind::Module)
            .expect("module code without a module scope") as u32
    }

    fn module_slot_of(&self, code: &CodeBlock, name: &str) -> Option<usize> {
        let si = Self::module_scope_index(code) as usize;
        code.scopes[si].slot_of(name)
    }

    // ---- linking -------------------------------------------------------------------------------

    pub fn link_module(&mut self, module: ObjId) -> JsResult<()> {
        let mut stack = Vec::new();
        match self.inner_module_linking(module, &mut stack, 0) {
            Ok(_) => Ok(()),
            Err(f) => {
                for m in stack {
                    let r = record_mut(self, m);
                    r.status = ModuleStatus::Unlinked;
                    r.env = None;
                }
                Err(f)
            }
        }
    }

    fn inner_module_linking(
        &mut self,
        module: ObjId,
        stack: &mut Vec<ObjId>,
        mut index: usize,
    ) -> JsResult<usize> {
        let status = record(self, module).status;
        if matches!(
            status,
            ModuleStatus::Linking
                | ModuleStatus::Linked
                | ModuleStatus::EvaluatingAsync
                | ModuleStatus::Evaluated
        ) {
            return Ok(index);
        }
        if status == ModuleStatus::Evaluating {
            return Ok(index);
        }
        {
            let r = record_mut(self, module);
            r.status = ModuleStatus::Linking;
            r.dfs_index = index;
            r.dfs_ancestor_index = index;
        }
        index += 1;
        stack.push(module);
        let requested = record(self, module).requested.clone();
        for req in requested.into_iter().flatten() {
            index = self.inner_module_linking(req, stack, index)?;
            let rs = record(self, req).status;
            if rs == ModuleStatus::Linking {
                let anc = record(self, req).dfs_ancestor_index;
                let r = record_mut(self, module);
                r.dfs_ancestor_index = r.dfs_ancestor_index.min(anc);
            }
        }
        self.initialize_environment(module)?;
        let r = record(self, module);
        if r.dfs_ancestor_index == r.dfs_index {
            loop {
                let m = stack.pop().expect("linking stack underflow");
                record_mut(self, m).status = ModuleStatus::Linked;
                if m == module {
                    break;
                }
            }
        }
        Ok(index)
    }

    /// InitializeEnvironment(module).
    fn initialize_environment(&mut self, module: ObjId) -> JsResult<()> {
        let Some(code) = record(self, module).code.clone() else {
            return Ok(());
        };
        let info = code.module.clone().unwrap();
        // Indirect exports must resolve.
        for e in &info.indirect_exports {
            let name = e.export_name.clone().unwrap();
            match self.resolve_export(module, &name, &mut Vec::new()) {
                Resolution::Found(_) => {}
                Resolution::NotFound | Resolution::Ambiguous => {
                    let n = self.module_specifier_string(&name);
                    return self
                        .syntax_error(&format!("Export '{n}' is not defined or is ambiguous"));
                }
            }
        }
        let realm = record(self, module).realm;
        let global_env = self.realm_of(realm).global_env;
        let si = Self::module_scope_index(&code);
        let env = self.heap.alloc_declarative(Some((code.clone(), si)), Some(global_env));
        record_mut(self, module).env = Some(env);
        // Imports.
        for (i, imp) in info.imports.iter().enumerate() {
            let req = record(self, module).requested[imp.request].expect("unloaded module request");
            match &imp.import_name {
                None => {
                    let ns = self.get_module_namespace(req)?;
                    let slot = self
                        .module_slot_of(&code, &imp.local_name)
                        .expect("namespace import without a slot");
                    self.heap.env_mut(env).as_decl_mut().unwrap().slots[slot] = Value::Object(ns);
                }
                Some(import_name) => match self.resolve_export(req, import_name, &mut Vec::new()) {
                    Resolution::Found(b) => record_mut(self, module).import_bindings[i] = Some(b),
                    Resolution::NotFound | Resolution::Ambiguous => {
                        let n = self.module_specifier_string(import_name);
                        let spec =
                            self.module_specifier_string(&info.requested[imp.request].specifier);
                        return self.syntax_error(&format!(
                            "The requested module '{spec}' does not provide an export named '{n}'"
                        ));
                    }
                },
            }
        }
        // Function declarations are instantiated at link time.
        if let Some(decls) = &code.declarations {
            for (name, fidx) in &decls.functions {
                let fcode = code.functions[*fidx as usize].clone();
                let f = self.create_function(fcode, Some(env), Some(module));
                let slot = self
                    .module_slot_of(&code, name)
                    .expect("function declaration without a slot");
                self.heap.env_mut(env).as_decl_mut().unwrap().slots[slot] = Value::Object(f);
            }
        }
        // The module body as a function closed over the module environment.
        let f = self.create_function(code.clone(), Some(env), Some(module));
        record_mut(self, module).function = Some(f);
        Ok(())
    }

    // ---- evaluation ----------------------------------------------------------------------------

    /// Evaluate(module): returns the evaluation promise.
    pub fn evaluate_module(&mut self, mut module: ObjId) -> JsResult<ObjId> {
        let status = record(self, module).status;
        if matches!(
            status,
            ModuleStatus::EvaluatingAsync | ModuleStatus::Evaluated
        ) {
            module = record(self, module).cycle_root.unwrap_or(module);
        }
        if let Some(cap) = &record(self, module).top_level_capability {
            return Ok(cap.promise);
        }
        let cap = promise::new_promise_capability_intrinsic(self);
        record_mut(self, module).top_level_capability = Some(cap.clone());
        let mut stack = Vec::new();
        match self.inner_module_evaluation(module, &mut stack, 0) {
            Ok(_) => {
                let r = record(self, module);
                debug_assert!(matches!(
                    r.status,
                    ModuleStatus::EvaluatingAsync | ModuleStatus::Evaluated
                ));
                debug_assert!(r.error.is_none());
                if !r.async_evaluation {
                    promise::resolve_capability(self, &cap, Value::Undefined)?;
                }
            }
            Err(Fault::Throw(e)) => {
                for m in stack {
                    let r = record_mut(self, m);
                    r.status = ModuleStatus::Evaluated;
                    r.error = Some(e);
                }
                record_mut(self, module).status = ModuleStatus::Evaluated;
                record_mut(self, module).error = Some(e);
                promise::reject_capability(self, &cap, e)?;
            }
            Err(f) => return Err(f),
        }
        Ok(cap.promise)
    }

    fn inner_module_evaluation(
        &mut self,
        module: ObjId,
        stack: &mut Vec<ObjId>,
        mut index: usize,
    ) -> JsResult<usize> {
        if record(self, module).code.is_none() {
            // Synthetic module.
            record_mut(self, module).status = ModuleStatus::Evaluated;
            return Ok(index);
        }
        let status = record(self, module).status;
        match status {
            ModuleStatus::EvaluatingAsync | ModuleStatus::Evaluated => {
                return match record(self, module).error {
                    None => Ok(index),
                    Some(e) => Err(Fault::Throw(e)),
                };
            }
            ModuleStatus::Evaluating => return Ok(index),
            ModuleStatus::Linked => {}
            other => panic!("evaluating a module in state {other:?}"),
        }
        {
            let r = record_mut(self, module);
            r.status = ModuleStatus::Evaluating;
            r.dfs_index = index;
            r.dfs_ancestor_index = index;
            r.pending_async_deps = 0;
        }
        index += 1;
        stack.push(module);
        let requested = record(self, module).requested.clone();
        for req in requested.into_iter().flatten() {
            index = self.inner_module_evaluation(req, stack, index)?;
            let mut required = req;
            if record(self, required).status == ModuleStatus::Evaluating {
                let anc = record(self, required).dfs_ancestor_index;
                let r = record_mut(self, module);
                r.dfs_ancestor_index = r.dfs_ancestor_index.min(anc);
            } else {
                required = record(self, required).cycle_root.unwrap_or(required);
                if let Some(e) = record(self, required).error {
                    return Err(Fault::Throw(e));
                }
            }
            if record(self, required).async_evaluation {
                record_mut(self, module).pending_async_deps += 1;
                record_mut(self, required).async_parents.push(module);
            }
        }
        let (pending, has_tla) = {
            let r = record(self, module);
            (r.pending_async_deps, r.has_tla)
        };
        if pending > 0 || has_tla {
            self.async_order_counter += 1;
            let order = self.async_order_counter;
            let r = record_mut(self, module);
            r.async_evaluation = true;
            r.async_order = order;
            if pending == 0 {
                self.execute_async_module(module)?;
            }
        } else {
            self.execute_module(module)?;
        }
        let r = record(self, module);
        if r.dfs_ancestor_index == r.dfs_index {
            loop {
                let m = stack.pop().expect("evaluation stack underflow");
                let r = record_mut(self, m);
                r.status = if r.async_evaluation {
                    ModuleStatus::EvaluatingAsync
                } else {
                    ModuleStatus::Evaluated
                };
                r.cycle_root = Some(module);
                if m == module {
                    break;
                }
            }
        }
        Ok(index)
    }

    /// ExecuteModule(module) for a synchronous module body.
    fn execute_module(&mut self, module: ObjId) -> JsResult<()> {
        let f = record(self, module).function.expect("unlinked module");
        let saved = self.current_realm;
        self.current_realm = record(self, module).realm;
        let r = self.call_value(Value::Object(f), Value::Undefined, &[]);
        self.current_realm = saved;
        r.map(|_| ())
    }

    /// ExecuteAsyncModule(module).
    fn execute_async_module(&mut self, module: ObjId) -> JsResult<()> {
        let f = record(self, module).function.expect("unlinked module");
        let saved = self.current_realm;
        self.current_realm = record(self, module).realm;
        let p = self.call_value(Value::Object(f), Value::Undefined, &[]);
        self.current_realm = saved;
        let Value::Object(p) = p? else {
            unreachable!("async module body did not return a promise")
        };
        let on_ok = vm_closure(self, async_module_fulfilled, module);
        let on_err = vm_closure(self, async_module_rejected, module);
        promise::perform_then(self, p, on_ok, on_err, None)?;
        Ok(())
    }

    /// AsyncModuleExecutionFulfilled(module).
    fn async_module_execution_fulfilled(&mut self, module: ObjId) -> JsResult<()> {
        if record(self, module).status == ModuleStatus::Evaluated {
            debug_assert!(record(self, module).error.is_some());
            return Ok(());
        }
        {
            let r = record_mut(self, module);
            r.async_evaluation = false;
            r.status = ModuleStatus::Evaluated;
        }
        if let Some(cap) = record(self, module).top_level_capability.clone() {
            promise::resolve_capability(self, &cap, Value::Undefined)?;
        }
        let mut exec_list = Vec::new();
        self.gather_available_ancestors(module, &mut exec_list);
        exec_list.sort_by_key(|m| record(self, *m).async_order);
        for m in exec_list {
            if record(self, m).status == ModuleStatus::Evaluated {
                continue;
            }
            if record(self, m).has_tla {
                self.execute_async_module(m)?;
            } else {
                match self.execute_module(m) {
                    Ok(()) => {
                        {
                            let r = record_mut(self, m);
                            r.status = ModuleStatus::Evaluated;
                            r.async_evaluation = false;
                        }
                        if let Some(cap) = record(self, m).top_level_capability.clone() {
                            promise::resolve_capability(self, &cap, Value::Undefined)?;
                        }
                    }
                    Err(Fault::Throw(e)) => self.async_module_execution_rejected(m, e)?,
                    Err(f) => return Err(f),
                }
            }
        }
        Ok(())
    }

    fn gather_available_ancestors(&mut self, module: ObjId, exec_list: &mut Vec<ObjId>) {
        let parents = record(self, module).async_parents.clone();
        for m in parents {
            if exec_list.contains(&m) {
                continue;
            }
            let root = record(self, m).cycle_root.unwrap_or(m);
            if record(self, root).error.is_some() {
                continue;
            }
            let r = record_mut(self, m);
            r.pending_async_deps -= 1;
            if r.pending_async_deps == 0 {
                exec_list.push(m);
                if !r.has_tla {
                    self.gather_available_ancestors(m, exec_list);
                }
            }
        }
    }

    /// AsyncModuleExecutionRejected(module, error).
    fn async_module_execution_rejected(&mut self, module: ObjId, error: Value) -> JsResult<()> {
        if record(self, module).status == ModuleStatus::Evaluated {
            return Ok(());
        }
        {
            let r = record_mut(self, module);
            r.error = Some(error);
            r.status = ModuleStatus::Evaluated;
            r.async_evaluation = false;
        }
        // Leaf to root: this module's own promise rejects before its async
        // parents are told (AsyncModuleExecutionRejected steps 9 and 10).
        if let Some(cap) = record(self, module).top_level_capability.clone() {
            promise::reject_capability(self, &cap, error)?;
        }
        let parents = record(self, module).async_parents.clone();
        for m in parents {
            self.async_module_execution_rejected(m, error)?;
        }
        Ok(())
    }

    // ---- namespaces and bindings ---------------------------------------------------------------

    /// GetModuleNamespace(module).
    pub fn get_module_namespace(&mut self, module: ObjId) -> JsResult<ObjId> {
        if let Some(ns) = record(self, module).namespace {
            return Ok(ns);
        }
        let names = self.exported_names(module, &mut Vec::new());
        let mut exports: Vec<Vec<u16>> = Vec::new();
        for n in names {
            if let Resolution::Found(_) = self.resolve_export(module, &n, &mut Vec::new()) {
                exports.push(n);
            }
        }
        exports.sort();
        let export_ids: Vec<StrId> = exports
            .into_iter()
            .map(|u| self.heap.alloc_str_utf16(u))
            .collect();
        let data = NamespaceData {
            module,
            exports: export_ids,
        };
        let mut obj = ObjectData::new(None, Class::Namespace(Box::new(data)));
        obj.extensible = false;
        let ns = self.alloc(obj);
        let tag = self.str("Module");
        let key = PropKey::Sym(self.well_known.to_string_tag);
        self.define_raw(ns, key, tag, Attrs::NONE);
        record_mut(self, module).namespace = Some(ns);
        Ok(ns)
    }

    /// The current value of a resolved binding; `Empty` while in the TDZ.
    fn binding_value(&mut self, b: Binding) -> JsResult<Value> {
        match b {
            Binding::Namespace(m) => Ok(Value::Object(self.get_module_namespace(m)?)),
            Binding::Slot { module, slot } => {
                let r = record(self, module);
                if r.code.is_none() {
                    return Ok(r.synthetic_value.unwrap_or(Value::Undefined));
                }
                let Some(env) = r.env else {
                    return Ok(Value::Empty);
                };
                Ok(self.heap.env(env).as_decl().unwrap().slots[slot])
            }
        }
    }

    /// The export `name` of a namespace's module: `None` when not exported.
    fn namespace_export(&mut self, ns: ObjId, key: PropKey) -> JsResult<Option<Value>> {
        let (module, name) = {
            let Class::Namespace(d) = &self.heap.obj(ns).class else {
                unreachable!()
            };
            let module = d.module;
            let name = match key {
                PropKey::Str(s) => {
                    let found = d.exports.iter().any(|e| {
                        self.heap
                            .string(*e)
                            .units()
                            .eq(&self.heap.string(s).units())
                    });
                    if !found {
                        return Ok(None);
                    }
                    self.heap.string(s).units().to_vec()
                }
                PropKey::Index(i) => {
                    let text = utf16(&i.to_string());
                    let found = d
                        .exports
                        .iter()
                        .any(|e| self.heap.string(*e).units().eq_units(&text));
                    if !found {
                        return Ok(None);
                    }
                    text
                }
                PropKey::Sym(_) => return Ok(None),
            };
            (module, name)
        };
        let Resolution::Found(b) = self.resolve_export(module, &name, &mut Vec::new()) else {
            return Ok(None);
        };
        let v = self.binding_value(b)?;
        if v == Value::Empty {
            let n = String::from_utf16_lossy(&name);
            return self.reference_error(&format!("Cannot access '{n}' before initialization"));
        }
        Ok(Some(v))
    }
}

/// A record for already-compiled module code (bytecode loading).
pub fn new_record(vm: &mut Vm, realm: RealmId, specifier: &str, code: Rc<CodeBlock>) -> ObjId {
    vm.new_module_record(realm, specifier, Some(code), None)
}

fn vm_closure(vm: &mut Vm, f: crate::object::NativeFn, module: ObjId) -> Value {
    Value::Object(vm.new_native_closure("", 1, f, vec![Value::Object(module)]))
}

fn async_module_fulfilled(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let m = vm.native_slot(a.callee, 0).as_object().unwrap();
    vm.async_module_execution_fulfilled(m)?;
    Ok(Value::Undefined)
}

fn async_module_rejected(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let m = vm.native_slot(a.callee, 0).as_object().unwrap();
    vm.async_module_execution_rejected(m, a.arg(0))?;
    Ok(Value::Undefined)
}

// ---- hooks called by the interpreter --------------------------------------------------------

/// `LdaImport(idx)`: the value of named import `idx` of the running module.
pub fn load_import(vm: &mut Vm, fi: usize, idx: u32) -> JsResult<Value> {
    let module = vm.frames[fi].module.expect("LdaImport outside module code");
    let b = record(vm, module).import_bindings[idx as usize].expect("unresolved import binding");
    vm.binding_value(b)
}

/// `import.meta` of the running module.
pub fn import_meta(vm: &mut Vm, fi: usize) -> JsResult<Value> {
    let module = vm.frames[fi]
        .module
        .expect("import.meta outside module code");
    if let Some(m) = record(vm, module).import_meta {
        return Ok(Value::Object(m));
    }
    let meta = vm.new_object_with_proto(None);
    let realm = record(vm, module).realm;
    let specifier = record(vm, module).specifier.clone();
    let host = vm.host.clone();
    let api_meta = vm.to_api_value(Value::Object(meta));
    let r = host.import_meta(vm, realm, &specifier, &api_meta);
    if let Err(e) = r {
        return Err(vm.api_error_to_fault(e));
    }
    record_mut(vm, module).import_meta = Some(meta);
    Ok(Value::Object(meta))
}

/// `import(spec, options)`.
pub fn dynamic_import(vm: &mut Vm, fi: usize, spec: Value, options: Value) -> JsResult<Value> {
    let referrer = vm.frames[fi]
        .module
        .map(|m| record(vm, m).specifier.clone())
        .or_else(|| Some(vm.frames[fi].code.source.name.to_string()));
    let realm = vm.frames[fi].realm;
    let cap = promise::new_promise_capability_intrinsic(vm);
    let body = |vm: &mut Vm| -> JsResult<Value> {
        let s = vm.to_string(spec)?;
        let specifier = vm.heap.string(s).units().to_string_lossy();
        let mut attributes = Vec::new();
        if !options.is_undefined() {
            let Value::Object(o) = options else {
                return vm.type_error("The second argument of import() must be an object");
            };
            let with = vm.get(o, PropKey::Str(vm.names.with))?;
            if !with.is_undefined() {
                let Value::Object(w) = with else {
                    return vm.type_error("The 'with' option of import() must be an object");
                };
                let entries = vm.enumerable_own_properties(w, 2)?;
                for e in entries {
                    let Value::Object(pair) = e else { continue };
                    let k = vm.get(pair, PropKey::Index(0))?;
                    let v = vm.get(pair, PropKey::Index(1))?;
                    let Value::String(vs) = v else {
                        return vm.type_error("Import attribute values must be strings");
                    };
                    let key = vm.to_rust_string(k)?;
                    let value = vm.heap.string(vs).units().to_string_lossy();
                    attributes.push(ImportAttribute { key, value });
                }
                attributes.sort_by(|a, b| a.key.cmp(&b.key));
            }
        }
        let p = vm.load_and_evaluate_module(realm, referrer.as_deref(), &specifier, &attributes)?;
        Ok(Value::Object(p))
    };
    match body(vm) {
        Ok(Value::Object(p)) => {
            // Resolve with the namespace once evaluation settles.
            let specifier = vm.to_string(spec).ok();
            let module_key = specifier.map(|s| vm.heap.string(s).units().to_string_lossy());
            let key_v = module_key.map(|k| vm.str(&k)).unwrap_or(Value::Undefined);
            let ref_v = referrer
                .as_deref()
                .map(|r| vm.str(r))
                .unwrap_or(Value::Undefined);
            let on_ok = vm.new_native_closure(
                "",
                1,
                dynamic_import_fulfilled,
                vec![
                    Value::Object(cap.promise),
                    cap.resolve,
                    cap.reject,
                    Value::Number(realm.0 as f64),
                    key_v,
                    ref_v,
                ],
            );
            promise::perform_then(vm, p, Value::Object(on_ok), cap.reject, None)?;
        }
        Ok(_) => unreachable!(),
        Err(Fault::Throw(e)) => promise::reject_capability(vm, &cap, e)?,
        Err(f) => return Err(f),
    }
    Ok(Value::Object(cap.promise))
}

/// `import()`: argument coercion happens now; loading and evaluation are
/// deferred to a job so that a module which imports itself dynamically
/// during its own evaluation finishes first (HostLoadImportedModule is
/// asynchronous).
pub fn dynamic_import_deferred(
    vm: &mut Vm,
    fi: usize,
    spec: Value,
    options: Value,
) -> JsResult<Value> {
    let referrer = vm.frames[fi]
        .module
        .map(|m| record(vm, m).specifier.clone())
        .or_else(|| Some(vm.frames[fi].code.source.name.to_string()));
    let realm = vm.frames[fi].realm;
    let cap = promise::new_promise_capability_intrinsic(vm);
    // ToString(specifier) and option validation are synchronous.
    let check = |vm: &mut Vm| -> JsResult<Value> {
        let s = vm.to_string(spec)?;
        if !options.is_undefined() {
            let Value::Object(o) = options else {
                return vm.type_error("The second argument of import() must be an object");
            };
            let with = vm.get(o, PropKey::Str(vm.names.with))?;
            if !with.is_undefined() {
                let Value::Object(w) = with else {
                    return vm.type_error("The 'with' option of import() must be an object");
                };
                let entries = vm.enumerable_own_properties(w, 2)?;
                for e in entries {
                    let Value::Object(pair) = e else { continue };
                    let v = vm.get(pair, PropKey::Index(1))?;
                    if !matches!(v, Value::String(_)) {
                        return vm.type_error("Import attribute values must be strings");
                    }
                }
            }
        }
        Ok(Value::String(s))
    };
    match check(vm) {
        Ok(spec_s) => {
            let ref_v = referrer
                .as_deref()
                .map(|r| vm.str(r))
                .unwrap_or(Value::Undefined);
            let job = vm.new_native_closure(
                "",
                0,
                dynamic_import_job,
                vec![
                    Value::Object(cap.promise),
                    cap.resolve,
                    cap.reject,
                    Value::Number(realm.0 as f64),
                    spec_s,
                    ref_v,
                    options,
                ],
            );
            vm.jobs.push_back(crate::vm::Job::Call {
                func: Value::Object(job),
                args: Vec::new(),
            });
        }
        Err(Fault::Throw(e)) => promise::reject_capability(vm, &cap, e)?,
        Err(f) => return Err(f),
    }
    Ok(Value::Object(cap.promise))
}

fn dynamic_import_job(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let promise = vm.native_slot(a.callee, 0);
    let resolve = vm.native_slot(a.callee, 1);
    let reject = vm.native_slot(a.callee, 2);
    let realm = RealmId(vm.native_slot(a.callee, 3).as_number().unwrap() as u32);
    let spec = vm.native_slot(a.callee, 4);
    let referrer = vm.native_slot(a.callee, 5);
    let options = vm.native_slot(a.callee, 6);
    let referrer_s = if referrer.is_undefined() {
        None
    } else {
        Some(vm.to_rust_string(referrer)?)
    };
    let Value::String(spec_sid) = spec else {
        unreachable!()
    };
    let specifier = vm.heap.string(spec_sid).units().to_string_lossy();
    let body = |vm: &mut Vm| -> JsResult<ObjId> {
        let mut attributes = Vec::new();
        if let Value::Object(o) = options {
            let with = vm.get(o, PropKey::Str(vm.names.with))?;
            if let Value::Object(w) = with {
                let entries = vm.enumerable_own_properties(w, 2)?;
                for e in entries {
                    let Value::Object(pair) = e else { continue };
                    let k = vm.get(pair, PropKey::Index(0))?;
                    let v = vm.get(pair, PropKey::Index(1))?;
                    let Value::String(vs) = v else {
                        return vm.type_error("Import attribute values must be strings");
                    };
                    let key = vm.to_rust_string(k)?;
                    let value = vm.heap.string(vs).units().to_string_lossy();
                    attributes.push(ImportAttribute { key, value });
                }
                attributes.sort_by(|a, b| a.key.cmp(&b.key));
            }
        }
        vm.load_and_evaluate_module(realm, referrer_s.as_deref(), &specifier, &attributes)
    };
    match body(vm) {
        Ok(p) => {
            let on_ok = vm.new_native_closure(
                "",
                1,
                dynamic_import_fulfilled,
                vec![
                    promise,
                    resolve,
                    reject,
                    Value::Number(realm.0 as f64),
                    spec,
                    referrer,
                ],
            );
            promise::perform_then(vm, p, Value::Object(on_ok), reject, None)?;
        }
        Err(Fault::Throw(e)) => {
            vm.call_value(reject, Value::Undefined, &[e])?;
        }
        Err(f) => return Err(f),
    }
    Ok(Value::Undefined)
}

fn dynamic_import_fulfilled(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let resolve = vm.native_slot(a.callee, 1);
    let reject = vm.native_slot(a.callee, 2);
    let realm = RealmId(vm.native_slot(a.callee, 3).as_number().unwrap() as u32);
    let spec = vm.native_slot(a.callee, 4);
    let referrer = vm.native_slot(a.callee, 5);
    let spec_s = vm.to_rust_string(spec)?;
    let referrer_s = if referrer.is_undefined() {
        None
    } else {
        Some(vm.to_rust_string(referrer)?)
    };
    let host = vm.host.clone();
    let resolved = match host.resolve_module(referrer_s.as_deref(), &spec_s, &[]) {
        Ok(r) => r,
        Err(msg) => {
            let e = vm.make_error(ErrorType::TypeError, &msg);
            vm.call_value(reject, Value::Undefined, &[e])?;
            return Ok(Value::Undefined);
        }
    };
    let Some(module) = vm.modules.get(&(realm, resolved)).copied() else {
        let e = vm.make_error(ErrorType::TypeError, "module record disappeared");
        vm.call_value(reject, Value::Undefined, &[e])?;
        return Ok(Value::Undefined);
    };
    match vm.get_module_namespace(module) {
        Ok(ns) => {
            vm.call_value(resolve, Value::Undefined, &[Value::Object(ns)])?;
        }
        Err(Fault::Throw(e)) => {
            vm.call_value(reject, Value::Undefined, &[e])?;
        }
        Err(f) => return Err(f),
    }
    Ok(Value::Undefined)
}

// ---- namespace exotic object -----------------------------------------------------------------

pub fn ns_get_own_property(vm: &mut Vm, o: ObjId, key: PropKey) -> JsResult<Option<Property>> {
    if let PropKey::Sym(_) = key {
        return Ok(vm.heap.get_own_property(o, key));
    }
    match vm.namespace_export(o, key)? {
        None => Ok(None),
        Some(v) => Ok(Some(Property::data(
            v,
            Attrs::from_flags(true, true, false),
        ))),
    }
}

pub fn ns_define_own_property(
    vm: &mut Vm,
    o: ObjId,
    key: PropKey,
    desc: PropertyDescriptor,
) -> JsResult<bool> {
    if let PropKey::Sym(_) = key {
        return Ok(vm.ordinary_define_own_property(o, key, desc));
    }
    let Some(current) = vm.namespace_export(o, key)? else {
        return Ok(false);
    };
    if desc.configurable == Some(true)
        || desc.enumerable == Some(false)
        || desc.is_accessor()
        || desc.writable == Some(false)
    {
        return Ok(false);
    }
    match desc.value {
        Some(v) => Ok(vm.same_value(v, current)),
        None => Ok(true),
    }
}

pub fn ns_get(vm: &mut Vm, o: ObjId, key: PropKey, receiver: Value) -> JsResult<Value> {
    if let PropKey::Sym(_) = key {
        return match vm.heap.get_own_property(o, key) {
            Some(p) => match p.slot {
                crate::value::PropSlot::Data(v) => Ok(v),
                crate::value::PropSlot::Accessor { get, .. } => {
                    if get.is_undefined() {
                        Ok(Value::Undefined)
                    } else {
                        vm.call_value(get.value(), receiver, &[])
                    }
                }
            },
            None => Ok(Value::Undefined),
        };
    }
    Ok(vm.namespace_export(o, key)?.unwrap_or(Value::Undefined))
}

pub fn ns_has_property(vm: &mut Vm, o: ObjId, key: PropKey) -> bool {
    if let PropKey::Sym(_) = key {
        return vm.heap.get_own_property(o, key).is_some();
    }
    let Class::Namespace(d) = &vm.heap.obj(o).class else {
        unreachable!()
    };
    let text = match key {
        PropKey::Str(s) => vm.heap.string(s).units().to_vec(),
        PropKey::Index(i) => utf16(&i.to_string()),
        PropKey::Sym(_) => unreachable!(),
    };
    d.exports
        .iter()
        .any(|e| vm.heap.string(*e).units().eq_units(&text))
}

pub fn ns_delete(vm: &mut Vm, o: ObjId, key: PropKey) -> JsResult<bool> {
    if let PropKey::Sym(_) = key {
        return Ok(vm.ordinary_delete(o, key));
    }
    let Class::Namespace(d) = &vm.heap.obj(o).class else {
        unreachable!()
    };
    let text = match key {
        PropKey::Str(s) => vm.heap.string(s).units().to_vec(),
        PropKey::Index(i) => utf16(&i.to_string()),
        PropKey::Sym(_) => unreachable!(),
    };
    let exported = d
        .exports
        .iter()
        .any(|e| vm.heap.string(*e).units().eq_units(&text));
    Ok(!exported)
}

pub fn ns_own_keys(vm: &mut Vm, o: ObjId) -> JsResult<Vec<PropKey>> {
    let exports = match &vm.heap.obj(o).class {
        Class::Namespace(d) => d.exports.clone(),
        _ => unreachable!(),
    };
    let mut keys: Vec<PropKey> = exports.into_iter().map(|s| vm.key_from_str(s)).collect();
    keys.extend(
        vm.heap
            .own_keys(o)
            .into_iter()
            .filter(|k| matches!(k, PropKey::Sym(_))),
    );
    Ok(keys)
}

/// `.then` handler of the host-facing module promise: yields the namespace.
fn resolve_with_namespace(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Value::Object(module) = vm.native_slot(a.callee, 0) else {
        unreachable!()
    };
    Ok(Value::Object(vm.get_module_namespace(module)?))
}
