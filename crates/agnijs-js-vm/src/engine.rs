//! The `agnijs_engine_api::Engine` implementation: value conversion across
//! the interface, host functions and objects, and the bytecode container.
//!
//! Bytecode container (ADR 0010): the shared `AGJS` container with the
//! `agnijs-bc-5` tag around a kind byte, a checksum and the serialised code
//! tree, so loading precompiled code skips parsing and compiling. Any other
//! container, including the M2 source-payload container, is rejected; the
//! host recompiles from source.

use crate::object::{CallArgs, Class, HostObjectData, NativeData, NativeKind, ObjectData};
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, PropKey, Property, Value};
use crate::vm::{ErrorType, Fault, JsResult, Vm};
use agnijs_engine_api::{
    self as api, CallContext, Engine, EngineInfo, Error, ErrorKind, Exception, Handle, HeapKind,
    HostClassDef, HostClassId, HostFunction, HostObject, JsBigInt, JsString, MemoryUsage,
    PromiseState, PropertyKey, RealmId, StackFrame, TypeOf,
};
use agnijs_js_compiler::bytecode::CodeBlock;
use std::rc::Rc;

/// The format tag in the shared `AGJS` container (`agnijs_engine_api::bytecode`):
/// a kind byte, then the serialised code tree (`agnijs_js_compiler::serialize`).
/// The tag pins the compiler: any change to `Instr`, `CodeBlock`, the
/// constant pool or the serialiser's layout changes it.
pub const FORMAT: &str = "agnijs-bc-5";

const KIND_SCRIPT: u8 = 1;
const KIND_MODULE: u8 = 2;

pub enum Decoded {
    Script(Rc<CodeBlock>),
    Module(Rc<CodeBlock>),
}

/// 64-bit FNV-1a over little-endian 64-bit words (the last word zero-padded,
/// then the length): detects accidental damage (truncated downloads, bad
/// disks, a bundler that rewrote bytes) before the tree is decoded. Each step
/// multiplies by an odd constant, a bijection on `u64`, so two inputs that
/// differ in one word never collide; words instead of bytes make it eight
/// times shorter as a dependency chain. It is not a signature; bytecode is
/// code, trusted as far as source would be.
fn checksum(kind: u8, tree: &[u8]) -> u64 {
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ kind as u64;
    h = h.wrapping_mul(PRIME);
    let mut words = tree.chunks_exact(8);
    for w in &mut words {
        h ^= u64::from_le_bytes(w.try_into().unwrap());
        h = h.wrapping_mul(PRIME);
    }
    let rest = words.remainder();
    if !rest.is_empty() {
        let mut last = [0u8; 8];
        last[..rest.len()].copy_from_slice(rest);
        h ^= u64::from_le_bytes(last);
        h = h.wrapping_mul(PRIME);
    }
    (h ^ tree.len() as u64).wrapping_mul(PRIME)
}

impl Vm {
    /// With a debugger attached, the debug sections of a loaded tree are
    /// decoded and verified at load, so a malformed one is refused the way
    /// the rest of the payload is instead of surfacing later as missing
    /// statements or locals. Without one, they decode on first use.
    pub(crate) fn verify_debug_sections(&self, code: &CodeBlock) -> Result<(), Error> {
        if self.debug.is_none() {
            return Ok(());
        }
        fn walk(code: &CodeBlock) -> Result<(), Error> {
            code.debug
                .get()
                .map_err(|e| Error::BytecodeCorrupt(format!("{}: {e}", code.name)))?;
            code.functions.iter().try_for_each(|f| walk(f))
        }
        walk(code)
    }
}

/// Payload: kind byte, checksum of kind and tree (u64 LE), tree.
pub fn encode_bytecode(module: bool, code: &CodeBlock) -> Vec<u8> {
    let kind = if module { KIND_MODULE } else { KIND_SCRIPT };
    let tree = agnijs_js_compiler::serialize::encode(code);
    let mut payload = Vec::with_capacity(9 + tree.len());
    payload.push(kind);
    payload.extend_from_slice(&checksum(kind, &tree).to_le_bytes());
    payload.extend_from_slice(&tree);
    api::bytecode::wrap(FORMAT, &payload)
}

pub fn decode_bytecode(bytes: &[u8], _want_module: bool) -> Result<Decoded, Error> {
    let payload = api::bytecode::unwrap(FORMAT, bytes)?;
    if payload.len() < 9 {
        return Err(Error::BytecodeCorrupt("truncated payload header".into()));
    }
    let kind = payload[0];
    let sum = u64::from_le_bytes(payload[1..9].try_into().unwrap());
    let tree = &payload[9..];
    if checksum(kind, tree) != sum {
        return Err(Error::BytecodeCorrupt("payload checksum mismatch".into()));
    }
    let code = agnijs_js_compiler::serialize::decode(tree)
        .map_err(|e| Error::BytecodeCorrupt(e.to_string()))?;
    use agnijs_js_compiler::bytecode::CodeKind;
    match (kind, code.kind) {
        (KIND_SCRIPT, CodeKind::Script) => Ok(Decoded::Script(code)),
        (KIND_MODULE, CodeKind::Module) => Ok(Decoded::Module(code)),
        (KIND_SCRIPT | KIND_MODULE, k) => Err(Error::BytecodeCorrupt(format!(
            "kind byte {kind} does not match a {k:?} code block"
        ))),
        _ => Err(Error::BytecodeCorrupt("unknown code kind".into())),
    }
}

// ---- value conversion -------------------------------------------------------------------------

impl Vm {
    pub fn to_api_value(&mut self, v: Value) -> api::Value {
        match v {
            Value::Undefined | Value::Empty => api::Value::Undefined,
            Value::Null => api::Value::Null,
            Value::Bool(b) => api::Value::Bool(b),
            Value::Number(n) => api::Value::Number(n),
            Value::String(s) => {
                self.heap.flatten(s);
                api::Value::String(JsString::from_utf16(self.heap.string(s).units().to_vec()))
            }
            Value::BigInt(b) => api::Value::BigInt(JsBigInt(self.heap.bigint(b).0.to_string())),
            Value::Symbol(_) => api::Value::Symbol(self.handles.insert(HeapKind::Symbol, v)),
            Value::Object(_) => api::Value::Object(self.handles.insert(HeapKind::Object, v)),
        }
    }

    pub fn from_api_value(&mut self, v: &api::Value) -> api::Result<Value> {
        Ok(match v {
            api::Value::Undefined => Value::Undefined,
            api::Value::Null => Value::Null,
            api::Value::Bool(b) => Value::Bool(*b),
            api::Value::Number(n) => Value::Number(*n),
            api::Value::String(s) => Value::String(self.heap.alloc_str_utf16(s.units().to_vec())),
            api::Value::BigInt(b) => {
                let n: num_bigint::BigInt = b
                    .0
                    .parse()
                    .map_err(|_| Error::Internal(format!("invalid BigInt literal {:?}", b.0)))?;
                self.bigint_value(n)
            }
            api::Value::Symbol(h) | api::Value::Object(h) => self.handle_value(h)?,
        })
    }

    fn handle_value(&self, h: &Handle) -> api::Result<Value> {
        self.handles.get(h).copied().ok_or(Error::ForeignHandle)
    }

    fn api_object(&mut self, v: &api::Value) -> api::Result<ObjId> {
        match self.from_api_value(v)? {
            Value::Object(o) => Ok(o),
            other => {
                let desc = self.describe_for_error(other);
                Err(Error::Internal(format!("expected an object, got {desc}")))
            }
        }
    }

    fn api_key(&mut self, k: &PropertyKey) -> api::Result<PropKey> {
        Ok(match k {
            PropertyKey::String(s) => {
                let id = self.heap.alloc_str_utf16(s.units().to_vec());
                self.key_from_str(id)
            }
            PropertyKey::Symbol(h) => match self.handle_value(h)? {
                Value::Symbol(s) => PropKey::Sym(s),
                _ => {
                    return Err(Error::Internal(
                        "symbol handle does not name a symbol".into(),
                    ))
                }
            },
        })
    }

    fn key_to_api(&mut self, k: PropKey) -> PropertyKey {
        match k {
            PropKey::Sym(s) => {
                PropertyKey::Symbol(self.handles.insert(HeapKind::Symbol, Value::Symbol(s)))
            }
            other => {
                let s = self.key_to_string(other);
                self.heap.flatten(s);
                PropertyKey::String(JsString::from_utf16(self.heap.string(s).units().to_vec()))
            }
        }
    }

    /// Turn a host-side error into a fault inside the running realm.
    pub fn api_error_to_fault(&mut self, e: Error) -> Fault {
        match e {
            Error::Exception(ex) => match self.from_api_value(&ex.value) {
                Ok(v) => Fault::Throw(v),
                Err(_) => self.throw(ErrorType::Error, &ex.message),
            },
            Error::Interrupted => Fault::Interrupt,
            Error::OutOfMemory => Fault::OutOfMemory,
            Error::ForeignHandle => self.throw_type("handle belongs to another engine"),
            Error::UnknownRealm => self.throw_type("unknown realm"),
            Error::BytecodeMismatch { expected, found } => self.throw_type(&format!(
                "bytecode format mismatch: expected {expected}, found {found}"
            )),
            Error::BytecodeCorrupt(m) => self.throw_type(&format!("corrupt bytecode: {m}")),
            Error::Unsupported(m) => self.throw_type(m),
            Error::Host(m) | Error::Internal(m) => self.throw(ErrorType::Error, &m),
        }
    }

    /// Turn a fault that escaped to the host into an API error.
    pub fn fault_to_error(&mut self, f: Fault) -> Error {
        match f {
            Fault::Interrupt => Error::Interrupted,
            Fault::OutOfMemory => Error::OutOfMemory,
            Fault::Throw(v) => {
                let (name, message) = self.exception_name_message(v);
                let stack = std::mem::take(&mut self.throw_stack);
                self.throw_stack_value = None;
                let value = self.to_api_value(v);
                Error::Exception(Exception {
                    value,
                    name,
                    message,
                    stack,
                })
            }
        }
    }

    /// `name` and `message` of a thrown value, without running user code
    /// that could throw again.
    pub(crate) fn exception_name_message(&mut self, v: Value) -> (String, String) {
        match v {
            Value::Object(o) => {
                let name_key = PropKey::Str(self.names.name);
                let name = match self.lookup_data_property(o, name_key) {
                    Some(Value::String(s)) => self.heap.str_to_string(s),
                    _ => "Error".to_string(),
                };
                let msg_key = PropKey::Str(self.names.message);
                let message = match self.lookup_data_property(o, msg_key) {
                    Some(Value::String(s)) => self.heap.str_to_string(s),
                    Some(Value::Undefined) | None => String::new(),
                    Some(other) => self.display(other),
                };
                (name, message)
            }
            Value::String(s) => (String::new(), self.heap.str_to_string(s)),
            other => (String::new(), self.display(other)),
        }
    }

    /// Prototype-chain lookup that reads data properties only.
    pub(crate) fn lookup_data_property(&self, mut o: ObjId, key: PropKey) -> Option<Value> {
        for _ in 0..64 {
            if let Some(p) = self.heap.get_own_property(o, key) {
                return p.value();
            }
            o = self.heap.obj(o).proto?;
        }
        None
    }

    /// Record the frames unwound while `fault` propagates (innermost first).
    pub(crate) fn note_unwound_frame(&mut self, fault: &Fault, frame: &crate::interp::Frame) {
        let Fault::Throw(v) = fault else { return };
        if self.throw_stack_value != Some(*v) {
            self.throw_stack.clear();
            self.throw_stack_value = Some(*v);
        }
        if self.throw_stack.len() >= 64 {
            return;
        }
        let (line, column) = Vm::line_col(&frame.code, frame.pc);
        let function = match frame.callee {
            Some(_) => frame.code.name.to_string(),
            None => String::new(),
        };
        self.throw_stack.push(StackFrame {
            function,
            script: frame.code.source.name.to_string(),
            line,
            column,
        });
    }

    fn check_realm(&self, realm: RealmId) -> api::Result<()> {
        if (realm.0 as usize) < self.realms.len() {
            Ok(())
        } else {
            Err(Error::UnknownRealm)
        }
    }

    /// Run `f` as an entry from the host into `realm`.
    fn enter<T>(
        &mut self,
        realm: RealmId,
        f: impl FnOnce(&mut Vm) -> JsResult<T>,
    ) -> api::Result<T> {
        self.check_realm(realm)?;
        self.handles.reclaim();
        let saved = self.current_realm;
        self.current_realm = realm;
        let r = f(self);
        self.current_realm = saved;
        let r = match r {
            Ok(v) => Ok(v),
            Err(fault) => Err(self.fault_to_error(fault)),
        };
        if self.debug_hot && self.frames.is_empty() {
            self.debug_run_ended(r.as_ref().err());
        }
        r
    }

    /// A collection safe point on entry to a host call, before any argument
    /// is turned into an unrooted VM value: only the host's handles are
    /// live. The called function runs as a native frame, where collection
    /// is deferred, so an embedder that drives code through host calls
    /// (timers, event handlers) would otherwise never collect.
    fn host_call_safe_point(&mut self) {
        if self.native_depth == 0 && self.frames.is_empty() && (self.gc_requested || self.heap.wants_gc()) {
            self.handles.reclaim();
            self.collect();
        }
    }

    fn syntax_error_value(&mut self, realm: RealmId, message: &str) -> Error {
        let saved = self.current_realm;
        self.current_realm = realm;
        let v = self.make_error(ErrorType::SyntaxError, message);
        self.current_realm = saved;
        let e = self.fault_to_error(Fault::Throw(v));
        if self.debug_hot && self.frames.is_empty() {
            self.debug_run_ended(Some(&e));
        }
        e
    }

    /// Run host finalisers for host objects collected since the last call.
    pub fn run_pending_finalizers(&mut self) {
        let pending = std::mem::take(&mut self.pending_finalizers);
        for (class, data) in pending {
            if let Some(def) = self.host_classes.get(class.0 as usize) {
                if let Some(f) = &def.finalize {
                    f(data);
                }
            }
        }
    }
}

/// Call a host function from the interpreter.
pub fn call_host_function(
    vm: &mut Vm,
    h: &Rc<HostFunction>,
    this: Value,
    args: &[Value],
    new_target: Value,
) -> JsResult<Value> {
    let realm = vm.current_realm;
    let this_v = vm.to_api_value(this);
    let args_v: Vec<api::Value> = args.iter().map(|a| vm.to_api_value(*a)).collect();
    let new_target = if new_target.is_undefined() {
        None
    } else {
        Some(vm.to_api_value(new_target))
    };
    let cx = CallContext {
        realm,
        this: this_v,
        args: args_v,
        new_target,
    };
    match h(vm, cx) {
        Ok(v) => match vm.from_api_value(&v) {
            Ok(v) => Ok(v),
            Err(e) => Err(vm.api_error_to_fault(e)),
        },
        Err(e) => Err(vm.api_error_to_fault(e)),
    }
}

fn error_type(kind: ErrorKind) -> ErrorType {
    match kind {
        ErrorKind::Error => ErrorType::Error,
        ErrorKind::TypeError => ErrorType::TypeError,
        ErrorKind::RangeError => ErrorType::RangeError,
        ErrorKind::SyntaxError => ErrorType::SyntaxError,
        ErrorKind::ReferenceError => ErrorType::ReferenceError,
        ErrorKind::EvalError => ErrorType::EvalError,
        ErrorKind::UriError => ErrorType::URIError,
    }
}

impl Engine for Vm {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            name: "agnijs",
            bytecode_format: FORMAT,
        }
    }

    fn create_realm(&mut self) -> api::Result<RealmId> {
        Ok(Vm::create_realm(self))
    }

    fn main_realm(&self) -> RealmId {
        RealmId(0)
    }

    fn global_object(&mut self, realm: RealmId) -> api::Result<api::Value> {
        self.check_realm(realm)?;
        let g = self.realm_of(realm).global;
        Ok(self.to_api_value(Value::Object(g)))
    }

    fn eval_script(&mut self, realm: RealmId, name: &str, source: &str) -> api::Result<api::Value> {
        self.check_realm(realm)?;
        let code = match agnijs_js_compiler::compile_script(source, name) {
            Ok(c) => c,
            Err(e) => return Err(self.syntax_error_value(realm, &e.message)),
        };
        self.note_script(&code, agnijs_js_debugger::ScriptKind::Script, realm);
        self.realms[realm.0 as usize]
            .script_names
            .push(name.to_string());
        let v = self.enter(realm, |vm| vm.run_script(code))?;
        Ok(self.to_api_value(v))
    }

    fn load_module(&mut self, realm: RealmId, specifier: &str) -> api::Result<api::Value> {
        let p = self.enter(realm, |vm| {
            vm.load_and_evaluate_module(realm, None, specifier, &[])
        })?;
        Ok(self.to_api_value(Value::Object(p)))
    }

    fn release_module(&mut self, realm: RealmId, specifier: &str) -> api::Result<bool> {
        self.check_realm(realm)?;
        Ok(self.modules.remove(&(realm, specifier.to_string())).is_some())
    }

    fn run_jobs(&mut self, realm: RealmId) -> api::Result<usize> {
        self.run_pending_finalizers();
        self.enter(realm, |vm| vm.drain_jobs())
    }

    fn has_pending_jobs(&self) -> bool {
        !self.jobs.is_empty() || !self.pending_finalizers.is_empty()
    }

    fn script_source(&mut self, _realm: RealmId, name: &str) -> Option<String> {
        self.scripts.get(name).map(|s| s.code.source.text.to_string())
    }

    fn compile_script(&mut self, realm: RealmId, name: &str, source: &str) -> api::Result<Vec<u8>> {
        self.check_realm(realm)?;
        match agnijs_js_compiler::compile_script(source, name) {
            Ok(code) => Ok(encode_bytecode(false, &code)),
            Err(e) => Err(self.syntax_error_value(realm, &e.message)),
        }
    }

    fn compile_module(
        &mut self,
        realm: RealmId,
        specifier: &str,
        source: &str,
    ) -> api::Result<Vec<u8>> {
        self.check_realm(realm)?;
        match agnijs_js_compiler::compile_module(source, specifier) {
            Ok(code) => Ok(encode_bytecode(true, &code)),
            Err(e) => Err(self.syntax_error_value(realm, &e.message)),
        }
    }

    fn eval_bytecode(&mut self, realm: RealmId, bytecode: &[u8]) -> api::Result<api::Value> {
        self.check_realm(realm)?;
        let decoded = decode_bytecode(bytecode, false)?;
        match &decoded {
            Decoded::Script(code) | Decoded::Module(code) => self.verify_debug_sections(code)?,
        }
        match decoded {
            Decoded::Script(code) => {
                self.note_script(&code, agnijs_js_debugger::ScriptKind::Script, realm);
                let v = self.enter(realm, |vm| vm.run_script(code))?;
                Ok(self.to_api_value(v))
            }
            Decoded::Module(code) => {
                let name = code.source.name.to_string();
                let p = self.enter(realm, |vm| {
                    let m = match vm.modules.get(&(realm, name.clone())) {
                        Some(m) => *m,
                        None => {
                            let m = vm.new_module_record_public(realm, &name, code);
                            vm.modules.insert((realm, name.clone()), m);
                            m
                        }
                    };
                    vm.link_module(m)?;
                    vm.evaluate_module(m)
                })?;
                Ok(self.to_api_value(Value::Object(p)))
            }
        }
    }

    fn set_memory_limit(&mut self, bytes: Option<usize>) {
        self.heap.set_memory_limit(bytes);
    }

    fn memory_usage(&mut self) -> MemoryUsage {
        MemoryUsage {
            heap_bytes: self.heap.approx_live_bytes(),
            object_count: self.heap.objects.live(),
        }
    }

    fn collect_garbage(&mut self) {
        if self.native_depth == 0 && self.frames.is_empty() {
            self.handles.reclaim();
            self.collect();
        } else {
            self.request_gc();
        }
    }

    fn type_of(&mut self, value: &api::Value) -> TypeOf {
        match self.from_api_value(value) {
            Ok(Value::Undefined) | Ok(Value::Empty) => TypeOf::Undefined,
            Ok(Value::Null) => TypeOf::Null,
            Ok(Value::Bool(_)) => TypeOf::Boolean,
            Ok(Value::Number(_)) => TypeOf::Number,
            Ok(Value::String(_)) => TypeOf::String,
            Ok(Value::BigInt(_)) => TypeOf::BigInt,
            Ok(Value::Symbol(_)) => TypeOf::Symbol,
            Ok(v @ Value::Object(_)) => {
                if self.is_callable(v) {
                    TypeOf::Function
                } else {
                    TypeOf::Object
                }
            }
            Err(_) => TypeOf::Undefined,
        }
    }

    fn strict_equals(&mut self, a: &api::Value, b: &api::Value) -> bool {
        match (self.from_api_value(a), self.from_api_value(b)) {
            (Ok(x), Ok(y)) => Vm::strict_equals(self, x, y),
            _ => false,
        }
    }

    fn to_string(&mut self, realm: RealmId, value: &api::Value) -> api::Result<String> {
        let v = self.from_api_value(value)?;
        self.enter(realm, |vm| vm.to_rust_string(v))
    }

    fn to_number(&mut self, realm: RealmId, value: &api::Value) -> api::Result<f64> {
        let v = self.from_api_value(value)?;
        self.enter(realm, |vm| vm.to_number(v))
    }

    fn new_object(&mut self, realm: RealmId) -> api::Result<api::Value> {
        let o = self.enter(realm, |vm| Ok(vm.new_object()))?;
        Ok(self.to_api_value(Value::Object(o)))
    }

    fn new_array(&mut self, realm: RealmId, items: &[api::Value]) -> api::Result<api::Value> {
        let mut vals = Vec::with_capacity(items.len());
        for i in items {
            vals.push(self.from_api_value(i)?);
        }
        let o = self.enter(realm, |vm| Ok(vm.new_array(vals)))?;
        Ok(self.to_api_value(Value::Object(o)))
    }

    fn new_error(
        &mut self,
        realm: RealmId,
        kind: ErrorKind,
        message: &str,
    ) -> api::Result<api::Value> {
        let v = self.enter(realm, |vm| Ok(vm.make_error(error_type(kind), message)))?;
        Ok(self.to_api_value(v))
    }

    fn new_symbol(&mut self, realm: RealmId, description: &str) -> api::Result<api::Value> {
        let v = self.enter(realm, |vm| {
            let d = vm.heap.alloc_str(description);
            Ok(Value::Symbol(vm.heap.alloc_symbol(
                crate::symbol::SymbolData::new(Some(d), crate::symbol::SymbolKind::Normal),
            )))
        })?;
        Ok(self.to_api_value(v))
    }

    fn get(
        &mut self,
        realm: RealmId,
        object: &api::Value,
        key: &PropertyKey,
    ) -> api::Result<api::Value> {
        let o = self.api_object(object)?;
        let k = self.api_key(key)?;
        let v = self.enter(realm, |vm| vm.get(o, k))?;
        Ok(self.to_api_value(v))
    }

    fn set(
        &mut self,
        realm: RealmId,
        object: &api::Value,
        key: &PropertyKey,
        value: &api::Value,
    ) -> api::Result<()> {
        let o = self.api_object(object)?;
        let k = self.api_key(key)?;
        let v = self.from_api_value(value)?;
        self.enter(realm, |vm| vm.set(o, k, v, true))
    }

    fn has(&mut self, realm: RealmId, object: &api::Value, key: &PropertyKey) -> api::Result<bool> {
        let o = self.api_object(object)?;
        let k = self.api_key(key)?;
        self.enter(realm, |vm| vm.has_property(o, k))
    }

    fn delete(
        &mut self,
        realm: RealmId,
        object: &api::Value,
        key: &PropertyKey,
    ) -> api::Result<bool> {
        let o = self.api_object(object)?;
        let k = self.api_key(key)?;
        self.enter(realm, |vm| vm.delete_property(o, k))
    }

    fn own_keys(&mut self, realm: RealmId, object: &api::Value) -> api::Result<Vec<PropertyKey>> {
        let o = self.api_object(object)?;
        let keys = self.enter(realm, |vm| {
            let all = vm.own_property_keys(o)?;
            let mut out = Vec::new();
            for k in all {
                if k.is_symbol() {
                    continue;
                }
                if let Some(p) = vm.own_property(o, k)? {
                    if p.attrs.enumerable() {
                        out.push(k);
                    }
                }
            }
            Ok(out)
        })?;
        Ok(keys.into_iter().map(|k| self.key_to_api(k)).collect())
    }

    fn call(
        &mut self,
        realm: RealmId,
        function: &api::Value,
        this: &api::Value,
        args: &[api::Value],
    ) -> api::Result<api::Value> {
        self.host_call_safe_point();
        let f = self.from_api_value(function)?;
        let t = self.from_api_value(this)?;
        let mut vals = Vec::with_capacity(args.len());
        for a in args {
            vals.push(self.from_api_value(a)?);
        }
        let v = self.enter(realm, |vm| {
            if !vm.is_callable(f) {
                return vm.type_error("value is not a function");
            }
            vm.call_value(f, t, &vals)
        })?;
        Ok(self.to_api_value(v))
    }

    fn construct(
        &mut self,
        realm: RealmId,
        constructor: &api::Value,
        args: &[api::Value],
    ) -> api::Result<api::Value> {
        self.host_call_safe_point();
        let c = self.from_api_value(constructor)?;
        let mut vals = Vec::with_capacity(args.len());
        for a in args {
            vals.push(self.from_api_value(a)?);
        }
        let v = self.enter(realm, |vm| {
            if !vm.is_constructor(c) {
                return vm.type_error("value is not a constructor");
            }
            vm.construct_value(c, &vals, c)
        })?;
        Ok(self.to_api_value(v))
    }

    fn is_array(&mut self, value: &api::Value) -> bool {
        match self.from_api_value(value) {
            Ok(v) => Vm::is_array(self, v).unwrap_or(false),
            Err(_) => false,
        }
    }

    fn is_promise(&mut self, value: &api::Value) -> bool {
        match self.from_api_value(value) {
            Ok(v) => crate::builtins::promise::is_promise(self, v),
            Err(_) => false,
        }
    }

    fn promise_state(
        &mut self,
        _realm: RealmId,
        promise: &api::Value,
    ) -> api::Result<PromiseState> {
        let o = self.api_object(promise)?;
        let (status, result) = match &self.heap.obj(o).class {
            Class::Promise(p) => (p.status, p.result),
            _ => return Err(Error::Internal("not a promise".into())),
        };
        Ok(match status {
            crate::builtins::promise::PromiseStatus::Pending => PromiseState::Pending,
            crate::builtins::promise::PromiseStatus::Fulfilled => {
                PromiseState::Fulfilled(self.to_api_value(result))
            }
            crate::builtins::promise::PromiseStatus::Rejected => {
                PromiseState::Rejected(self.to_api_value(result))
            }
        })
    }

    fn detach_array_buffer(&mut self, _realm: RealmId, buffer: &api::Value) -> api::Result<()> {
        let o = self.api_object(buffer)?;
        match &mut self.heap.obj_mut(o).class {
            Class::ArrayBuffer(b) => {
                if b.shared {
                    return Err(Error::Internal("cannot detach a SharedArrayBuffer".into()));
                }
                b.detached = true;
                b.bytes = Vec::new();
                Ok(())
            }
            _ => Err(Error::Internal("not an ArrayBuffer".into())),
        }
    }

    fn new_function(
        &mut self,
        realm: RealmId,
        name: &str,
        f: Rc<HostFunction>,
    ) -> api::Result<api::Value> {
        let o = self.enter(realm, |vm| {
            let proto = vm.intrinsic_in(realm, Intrinsic::FunctionPrototype);
            let data = NativeData {
                func: NativeKind::Host(f),
                tag: crate::object::NativeTag::None,
                realm,
                constructor: false,
                slots: Vec::new(),
                flag: None,
                counter: None,
            };
            let obj = vm.alloc(ObjectData::new(Some(proto), Class::Native(Box::new(data))));
            let len_key = PropKey::Str(vm.names.length);
            vm.heap.add_own_property(
                obj,
                len_key,
                Property::data(Value::Number(0.0), Attrs::CONFIGURABLE_ONLY),
            );
            let name_key = PropKey::Str(vm.names.name);
            let name_val = vm.str(name);
            vm.heap.add_own_property(
                obj,
                name_key,
                Property::data(name_val, Attrs::CONFIGURABLE_ONLY),
            );
            Ok(obj)
        })?;
        Ok(self.to_api_value(Value::Object(o)))
    }

    fn register_host_class(&mut self, def: HostClassDef) -> HostClassId {
        self.host_classes.push(def);
        HostClassId((self.host_classes.len() - 1) as u32)
    }

    fn new_host_object(
        &mut self,
        realm: RealmId,
        class: HostClassId,
        data: Box<dyn HostObject>,
        prototype: Option<&api::Value>,
    ) -> api::Result<api::Value> {
        let proto = match prototype {
            Some(p) => Some(self.api_object(p)?),
            None => None,
        };
        let o = self.enter(realm, |vm| {
            let proto = proto.unwrap_or_else(|| vm.intrinsic_in(realm, Intrinsic::ObjectPrototype));
            Ok(vm.alloc(ObjectData::new(
                Some(proto),
                Class::Host(Box::new(HostObjectData { class, data })),
            )))
        })?;
        Ok(self.to_api_value(Value::Object(o)))
    }

    fn host_object_data(&mut self, value: &api::Value) -> Option<&mut dyn HostObject> {
        let o = self.api_object(value).ok()?;
        match &mut self.heap.obj_mut(o).class {
            Class::Host(h) => Some(&mut *h.data),
            _ => None,
        }
    }

    fn host_object_class(&mut self, value: &api::Value) -> Option<HostClassId> {
        let o = self.api_object(value).ok()?;
        match &self.heap.obj(o).class {
            Class::Host(h) => Some(h.class),
            _ => None,
        }
    }

    fn reclaim_handles(&mut self) {
        self.handles.reclaim();
    }

    fn live_handles(&self) -> usize {
        self.handles.live()
    }
}

/// Construct the engine behind the interface.
pub fn make_engine(host: Rc<dyn api::Host>) -> Box<dyn Engine> {
    let mut vm = Vm::new(host);
    Vm::create_realm(&mut vm);
    Box::new(vm)
}

impl Vm {
    /// A module record for already-compiled module code (bytecode loading).
    pub fn new_module_record_public(
        &mut self,
        realm: RealmId,
        specifier: &str,
        code: Rc<CodeBlock>,
    ) -> ObjId {
        crate::modules::new_record(self, realm, specifier, code)
    }
}

/// `CallArgs` is re-exported for host-facing helpers.
pub type NativeCallArgs<'a> = CallArgs<'a>;
