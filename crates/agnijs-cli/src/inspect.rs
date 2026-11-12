//! `--inspect`: a debuggable engine behind the ordinary engine interface,
//! with a Chrome DevTools Protocol endpoint (ADR 0017).
//!
//! [`agnijs::Context`] owns a `Box<dyn Engine>`, while the CDP session needs
//! the engine as a [`Debuggee`]. [`Inspected`] is an [`Engine`] that forwards
//! every call to a debuggable engine and, at each entry point the command
//! uses (evaluation, calls, jobs), first serves the protocol messages that
//! arrived meanwhile. While JavaScript is paused the session serves messages
//! itself, from inside the engine.

use agnijs::api::{
    Engine, EngineInfo, ErrorKind, Handle, HostClassDef, HostClassId, HostFunction, HostObject,
    MemoryUsage, PromiseState, PropertyKey, RealmId, Result, TypeOf, Value,
};
use agnijs_js_cdp::{CdpServer, Session};
use agnijs_js_debugger::Debuggee;
use std::cell::RefCell;
use std::rc::Rc;

pub struct Inspected {
    debuggee: RefCell<Box<dyn Debuggee>>,
    session: Rc<Session>,
    /// `--inspect-brk` with a client ready: pause at the first statement of
    /// the next program evaluated that is not the command's own prelude.
    break_first: bool,
}

impl Inspected {
    /// Listen on `addr`, print the endpoint, and with `wait` block until a
    /// client sends `Runtime.runIfWaitingForDebugger` and then pause at the
    /// first statement of the program (`--inspect-brk`).
    pub fn start(
        mut debuggee: Box<dyn Debuggee>,
        addr: &str,
        title: &str,
        wait: bool,
    ) -> std::io::Result<Inspected> {
        let server = CdpServer::listen(addr, title)?;
        let local = server.local_addr();
        eprintln!("Debugger listening on {}", server.websocket_url());
        eprintln!(
            "Open chrome://inspect (or devtools://devtools/bundled/js_app.html?ws={}/{})",
            local,
            server
                .websocket_url()
                .rsplit('/')
                .next()
                .unwrap_or_default()
        );
        let realm = debuggee.engine().main_realm();
        let session = Session::new(server, realm);
        session.attach(&mut *debuggee);
        let mut break_first = false;
        if wait {
            eprintln!("Waiting for the debugger to connect...");
            break_first = session.wait_until_ready(&mut *debuggee);
        }
        Ok(Inspected {
            debuggee: RefCell::new(debuggee),
            session,
            break_first,
        })
    }

    /// Serve the protocol messages waiting, without blocking.
    fn poll(&mut self) {
        let d = self.debuggee.get_mut();
        self.session.poll(&mut **d);
    }

    /// Before evaluating `name`: serve messages, and request the
    /// `--inspect-brk` pause if this is the program.
    fn enter(&mut self, name: &str) {
        self.poll();
        if self.break_first && !name.starts_with("agnijs:") {
            self.break_first = false;
            self.debuggee.get_mut().pause_requester().request();
        }
    }

    fn e(&mut self) -> &mut dyn Engine {
        self.debuggee.get_mut().engine()
    }
}

impl Drop for Inspected {
    /// As Node does: once the program is done, a connected client keeps
    /// the process alive until it disconnects.
    fn drop(&mut self) {
        self.poll();
        if self.session.is_connected() {
            eprintln!("Waiting for the debugger to disconnect...");
            let d = self.debuggee.get_mut();
            self.session.serve(&mut **d);
        }
    }
}

impl Engine for Inspected {
    fn info(&self) -> EngineInfo {
        self.debuggee.borrow_mut().engine().info()
    }
    fn create_realm(&mut self) -> Result<RealmId> {
        self.e().create_realm()
    }
    fn main_realm(&self) -> RealmId {
        self.debuggee.borrow_mut().engine().main_realm()
    }
    fn global_object(&mut self, realm: RealmId) -> Result<Value> {
        self.e().global_object(realm)
    }
    fn eval_script(&mut self, realm: RealmId, name: &str, source: &str) -> Result<Value> {
        self.enter(name);
        self.e().eval_script(realm, name, source)
    }
    fn load_module(&mut self, realm: RealmId, specifier: &str) -> Result<Value> {
        self.enter(specifier);
        self.e().load_module(realm, specifier)
    }
    fn release_module(&mut self, realm: RealmId, specifier: &str) -> Result<bool> {
        self.e().release_module(realm, specifier)
    }
    fn run_jobs(&mut self, realm: RealmId) -> Result<usize> {
        self.poll();
        self.e().run_jobs(realm)
    }
    fn has_pending_jobs(&self) -> bool {
        self.debuggee.borrow_mut().engine().has_pending_jobs()
    }
    fn script_source(&mut self, realm: RealmId, name: &str) -> Option<String> {
        self.e().script_source(realm, name)
    }
    fn set_source_map(&mut self, script: &str, source_map: Option<&str>) -> Result<()> {
        self.e().set_source_map(script, source_map)
    }
    fn compile_script(&mut self, realm: RealmId, name: &str, source: &str) -> Result<Vec<u8>> {
        self.e().compile_script(realm, name, source)
    }
    fn compile_module(&mut self, realm: RealmId, specifier: &str, source: &str) -> Result<Vec<u8>> {
        self.e().compile_module(realm, specifier, source)
    }
    fn eval_bytecode(&mut self, realm: RealmId, bytecode: &[u8]) -> Result<Value> {
        self.enter("");
        self.e().eval_bytecode(realm, bytecode)
    }
    fn snapshot(&mut self, realm: RealmId) -> Result<Vec<u8>> {
        self.e().snapshot(realm)
    }
    fn set_memory_limit(&mut self, bytes: Option<usize>) {
        self.e().set_memory_limit(bytes)
    }
    fn memory_usage(&mut self) -> MemoryUsage {
        self.e().memory_usage()
    }
    fn set_fuel(&mut self, fuel: Option<u64>) -> Result<()> {
        self.e().set_fuel(fuel)
    }
    fn fuel(&self) -> Option<u64> {
        self.debuggee.borrow_mut().engine().fuel()
    }
    fn set_random_seed(&mut self, seed: u64) -> Result<()> {
        self.e().set_random_seed(seed)
    }
    fn set_stack_budget(&mut self, bytes: Option<usize>) -> Result<()> {
        self.e().set_stack_budget(bytes)
    }
    fn stack_budget(&self) -> Option<usize> {
        self.debuggee.borrow_mut().engine().stack_budget()
    }
    fn collect_garbage(&mut self) {
        self.e().collect_garbage()
    }
    fn type_of(&mut self, value: &Value) -> TypeOf {
        self.e().type_of(value)
    }
    fn strict_equals(&mut self, a: &Value, b: &Value) -> bool {
        self.e().strict_equals(a, b)
    }
    fn to_string(&mut self, realm: RealmId, value: &Value) -> Result<String> {
        self.e().to_string(realm, value)
    }
    fn to_number(&mut self, realm: RealmId, value: &Value) -> Result<f64> {
        self.e().to_number(realm, value)
    }
    fn new_object(&mut self, realm: RealmId) -> Result<Value> {
        self.e().new_object(realm)
    }
    fn new_array(&mut self, realm: RealmId, items: &[Value]) -> Result<Value> {
        self.e().new_array(realm, items)
    }
    fn new_error(&mut self, realm: RealmId, kind: ErrorKind, message: &str) -> Result<Value> {
        self.e().new_error(realm, kind, message)
    }
    fn new_symbol(&mut self, realm: RealmId, description: &str) -> Result<Value> {
        self.e().new_symbol(realm, description)
    }
    fn get(&mut self, realm: RealmId, object: &Value, key: &PropertyKey) -> Result<Value> {
        self.e().get(realm, object, key)
    }
    fn set(
        &mut self,
        realm: RealmId,
        object: &Value,
        key: &PropertyKey,
        value: &Value,
    ) -> Result<()> {
        self.e().set(realm, object, key, value)
    }
    fn has(&mut self, realm: RealmId, object: &Value, key: &PropertyKey) -> Result<bool> {
        self.e().has(realm, object, key)
    }
    fn delete(&mut self, realm: RealmId, object: &Value, key: &PropertyKey) -> Result<bool> {
        self.e().delete(realm, object, key)
    }
    fn own_keys(&mut self, realm: RealmId, object: &Value) -> Result<Vec<PropertyKey>> {
        self.e().own_keys(realm, object)
    }
    fn call(
        &mut self,
        realm: RealmId,
        function: &Value,
        this: &Value,
        args: &[Value],
    ) -> Result<Value> {
        self.poll();
        self.e().call(realm, function, this, args)
    }
    fn construct(&mut self, realm: RealmId, constructor: &Value, args: &[Value]) -> Result<Value> {
        self.e().construct(realm, constructor, args)
    }
    fn is_array(&mut self, value: &Value) -> bool {
        self.e().is_array(value)
    }
    fn is_promise(&mut self, value: &Value) -> bool {
        self.e().is_promise(value)
    }
    fn promise_state(&mut self, realm: RealmId, promise: &Value) -> Result<PromiseState> {
        self.e().promise_state(realm, promise)
    }
    fn detach_array_buffer(&mut self, realm: RealmId, buffer: &Value) -> Result<()> {
        self.e().detach_array_buffer(realm, buffer)
    }
    fn new_function(&mut self, realm: RealmId, name: &str, f: Rc<HostFunction>) -> Result<Value> {
        self.e().new_function(realm, name, f)
    }
    fn register_host_class(&mut self, def: HostClassDef) -> HostClassId {
        self.e().register_host_class(def)
    }
    fn new_host_object(
        &mut self,
        realm: RealmId,
        class: HostClassId,
        data: Box<dyn HostObject>,
        prototype: Option<&Value>,
    ) -> Result<Value> {
        self.e().new_host_object(realm, class, data, prototype)
    }
    fn host_object_data(&mut self, value: &Value) -> Option<&mut dyn HostObject> {
        self.debuggee.get_mut().engine().host_object_data(value)
    }
    fn host_object_class(&mut self, value: &Value) -> Option<HostClassId> {
        self.e().host_object_class(value)
    }
    fn reclaim_handles(&mut self) {
        self.e().reclaim_handles()
    }
    fn live_handles(&self) -> usize {
        self.debuggee.borrow_mut().engine().live_handles()
    }
    fn same_object(&mut self, a: &Handle, b: &Handle) -> bool {
        self.e().same_object(a, b)
    }
}
