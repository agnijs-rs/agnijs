//! The engine interface: what the embedder (the host) codes against.
//!
//! Every method takes `&mut self`. Engines are single-threaded and own their
//! heap; the host drives them from one thread. Host functions receive
//! `&mut dyn Engine` so they can re-enter.

use crate::error::Result;
use crate::host::{HostClassDef, HostClassId, HostFunction, HostObject};
use crate::value::{Handle, PromiseState, PropertyKey, RealmId, TypeOf, Value};
use std::rc::Rc;

/// The kind of a newly created Error object.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrorKind {
    Error,
    TypeError,
    RangeError,
    SyntaxError,
    ReferenceError,
    EvalError,
    UriError,
}

impl ErrorKind {
    pub fn name(self) -> &'static str {
        match self {
            ErrorKind::Error => "Error",
            ErrorKind::TypeError => "TypeError",
            ErrorKind::RangeError => "RangeError",
            ErrorKind::SyntaxError => "SyntaxError",
            ErrorKind::ReferenceError => "ReferenceError",
            ErrorKind::EvalError => "EvalError",
            ErrorKind::UriError => "URIError",
        }
    }
}

/// Memory statistics an engine can report; fields are zero when unknown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryUsage {
    pub heap_bytes: usize,
    pub object_count: usize,
}

/// Static facts about an engine implementation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineInfo {
    /// A short identifier, for example `quickjs-ng` or `agnijs`.
    pub name: &'static str,
    /// Bytecode format tag embedded in serialized output.
    pub bytecode_format: &'static str,
}

pub trait Engine {
    fn info(&self) -> EngineInfo;

    // ---- realms -------------------------------------------------------

    /// Create a new realm with the standard library installed.
    fn create_realm(&mut self) -> Result<RealmId>;
    /// The realm created with the engine.
    fn main_realm(&self) -> RealmId;
    fn global_object(&mut self, realm: RealmId) -> Result<Value>;

    // ---- evaluation ---------------------------------------------------

    /// Evaluate a classic script. `name` identifies the script to the
    /// debugger and in stack traces.
    ///
    /// Source from the host is a Rust `str`, so it cannot hold a lone
    /// surrogate; write one as an escape (`"\uD800"`). Source that script
    /// builds at run time (`eval`, `Function`) is a JavaScript string and
    /// keeps its lone surrogates.
    fn eval_script(&mut self, realm: RealmId, name: &str, source: &str) -> Result<Value>;

    /// Load and evaluate a module by specifier through the host's module
    /// hooks. Returns a promise for the module namespace, which settles once
    /// the host has run the pending jobs.
    fn load_module(&mut self, realm: RealmId, specifier: &str) -> Result<Value>;

    /// Forget the module record registered under `specifier` (the resolved
    /// specifier the host's loader received). The next import of that
    /// specifier loads and evaluates the module again; the old record and
    /// its exports are collected once nothing references them. Modules
    /// that imported the old record keep their bindings to it: HMR logic in
    /// JavaScript decides what to re-import. Returns `false` when the
    /// engine cannot release records (the interim engine keeps every module
    /// for the life of its context), in which case a reload needs a fresh
    /// realm.
    fn release_module(&mut self, realm: RealmId, specifier: &str) -> Result<bool>;

    /// Run every pending job (promise reactions and anything else the engine
    /// enqueued). The engine never runs jobs on its own. Returns the number
    /// of jobs run; an exception escaping a job stops the drain and is
    /// returned (remaining jobs stay queued).
    fn run_jobs(&mut self, realm: RealmId) -> Result<usize>;
    fn has_pending_jobs(&self) -> bool;

    /// The source text of a script or module previously evaluated under
    /// `name`, for the debugger and source-mapped exceptions.
    fn script_source(&mut self, realm: RealmId, name: &str) -> Option<String>;

    // ---- bytecode -----------------------------------------------------

    fn compile_script(&mut self, realm: RealmId, name: &str, source: &str) -> Result<Vec<u8>>;
    fn compile_module(&mut self, realm: RealmId, specifier: &str, source: &str) -> Result<Vec<u8>>;
    /// Evaluate bytecode from [`Engine::compile_script`].
    fn eval_bytecode(&mut self, realm: RealmId, bytecode: &[u8]) -> Result<Value>;

    // ---- snapshots ----------------------------------------------------

    /// A snapshot of `realm` after initialisation (its built-ins plus the
    /// bootstrap scripts the host ran in it), which the engine's crate
    /// restores into a new engine (for AgniJS,
    /// `agnijs_js_vm::make_engine_from_snapshot`), so startup is a load
    /// instead of a re-run. Taken between calls, with no jobs pending.
    /// State a snapshot cannot carry (host objects and functions, pending
    /// work, generators, modules) fails with
    /// [`crate::Error::SnapshotUnsupported`]; handles are not part of it.
    /// Engines without snapshots return [`crate::Error::Unsupported`].
    fn snapshot(&mut self, realm: RealmId) -> Result<Vec<u8>> {
        let _ = realm;
        Err(crate::error::Error::Unsupported("snapshots"))
    }

    // ---- limits and collection ---------------------------------------

    /// Cap the heap; allocations beyond it fail with an exception or
    /// [`crate::Error::OutOfMemory`]. `None` removes the cap.
    fn set_memory_limit(&mut self, bytes: Option<usize>);
    fn memory_usage(&mut self) -> MemoryUsage;

    /// Set the fuel budget: how many more bytecode instructions JavaScript
    /// may run (`None` removes the budget). Every instruction the engine
    /// dispatches, in any realm and at any nesting of host calls, costs one
    /// unit; once none is left the running call fails with
    /// [`crate::Error::OutOfFuel`], which JavaScript cannot catch, and so
    /// does every later call until the host sets a new budget. The budget
    /// spans calls: a host meters one call by setting a budget before it
    /// (and reads what is left with [`Engine::fuel`] after), and meters a
    /// realm by giving it an engine of its own. The count depends only on
    /// the program and the engine build, so exhaustion comes at the same
    /// instruction on every run and platform. A native built-in costs only
    /// the instruction that calls it (and the instructions of any
    /// JavaScript it calls back); the memory limit and the interrupt hook
    /// bound the rest. Engines without fuel return
    /// [`crate::Error::Unsupported`].
    fn set_fuel(&mut self, fuel: Option<u64>) -> Result<()> {
        let _ = fuel;
        Err(crate::error::Error::Unsupported("fuel"))
    }
    /// The fuel left, or `None` when no budget is set.
    fn fuel(&self) -> Option<u64> {
        None
    }
    /// Reseed `Math.random` now: the values it gives from here on depend
    /// only on `seed` and how many it gives, the same on every run and
    /// platform. All realms of the engine draw from one stream. Until the
    /// first draw or a reseed the seed is [`crate::Host::random_seed`].
    /// Engines that cannot be seeded return [`crate::Error::Unsupported`].
    fn set_random_seed(&mut self, seed: u64) -> Result<()> {
        let _ = seed;
        Err(crate::error::Error::Unsupported("random seed"))
    }
    /// Run a full collection now.
    fn collect_garbage(&mut self);

    // ---- values -------------------------------------------------------

    fn type_of(&mut self, value: &Value) -> TypeOf;
    fn strict_equals(&mut self, a: &Value, b: &Value) -> bool;
    /// `ToString`; may call user code. The result is a Rust `String`, so
    /// each lone surrogate becomes U+FFFD (as [`crate::JsString::to_string_lossy`]);
    /// a host that needs the exact code units reads a [`Value::String`]
    /// instead, which keeps them both ways.
    fn to_string(&mut self, realm: RealmId, value: &Value) -> Result<String>;
    /// `ToNumber`; may call user code.
    fn to_number(&mut self, realm: RealmId, value: &Value) -> Result<f64>;

    fn new_object(&mut self, realm: RealmId) -> Result<Value>;
    fn new_array(&mut self, realm: RealmId, items: &[Value]) -> Result<Value>;
    fn new_error(&mut self, realm: RealmId, kind: ErrorKind, message: &str) -> Result<Value>;
    fn new_symbol(&mut self, realm: RealmId, description: &str) -> Result<Value>;

    fn get(&mut self, realm: RealmId, object: &Value, key: &PropertyKey) -> Result<Value>;
    fn set(
        &mut self,
        realm: RealmId,
        object: &Value,
        key: &PropertyKey,
        value: &Value,
    ) -> Result<()>;
    fn has(&mut self, realm: RealmId, object: &Value, key: &PropertyKey) -> Result<bool>;
    fn delete(&mut self, realm: RealmId, object: &Value, key: &PropertyKey) -> Result<bool>;
    /// Own enumerable string keys, in property order.
    fn own_keys(&mut self, realm: RealmId, object: &Value) -> Result<Vec<PropertyKey>>;

    fn call(
        &mut self,
        realm: RealmId,
        function: &Value,
        this: &Value,
        args: &[Value],
    ) -> Result<Value>;
    fn construct(&mut self, realm: RealmId, constructor: &Value, args: &[Value]) -> Result<Value>;

    fn is_array(&mut self, value: &Value) -> bool;
    fn is_promise(&mut self, value: &Value) -> bool;
    fn promise_state(&mut self, realm: RealmId, promise: &Value) -> Result<PromiseState>;
    /// `DetachArrayBuffer`.
    fn detach_array_buffer(&mut self, realm: RealmId, buffer: &Value) -> Result<()>;

    // ---- host functions and classes ---------------------------------

    fn new_function(&mut self, realm: RealmId, name: &str, f: Rc<HostFunction>) -> Result<Value>;
    fn register_host_class(&mut self, def: HostClassDef) -> HostClassId;
    /// Create an instance of a host class wrapping `data`, with `prototype`
    /// (or `Object.prototype` when `None`).
    fn new_host_object(
        &mut self,
        realm: RealmId,
        class: HostClassId,
        data: Box<dyn HostObject>,
        prototype: Option<&Value>,
    ) -> Result<Value>;
    /// Borrow the data of a host object; `None` for other values.
    fn host_object_data(&mut self, value: &Value) -> Option<&mut dyn HostObject>;
    fn host_object_class(&mut self, value: &Value) -> Option<HostClassId>;

    /// Release handles the host has dropped. Engines also do this at every
    /// entry point; calling it explicitly is only needed before a
    /// collection the host wants to be complete.
    fn reclaim_handles(&mut self);

    /// Number of live handles (for leak tests).
    fn live_handles(&self) -> usize;

    /// Compare two handles for JS identity. Default goes through
    /// [`Engine::strict_equals`].
    fn same_object(&mut self, a: &Handle, b: &Handle) -> bool {
        self.strict_equals(&Value::Object(a.clone()), &Value::Object(b.clone()))
    }
}
