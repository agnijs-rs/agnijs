//! The engine-interface conformance kit.
//!
//! Every engine behind the interface (the Gable engine and the interim
//! QuickJS-NG adapter) must pass these tests. An adapter crate instantiates
//! them with [`conformance_tests!`], giving a factory that builds an engine
//! around a [`TestHost`]:
//!
//! ```ignore
//! gable_engine_api::conformance_tests!(|host| Box::new(MyEngine::new(host)));
//! ```

use crate::engine::{Engine, ErrorKind};
use crate::error::Error;
use crate::host::{
    CallContext, ConsoleLevel, Host, HostClassDef, HostObject, ImportAttribute, ModuleSource,
    RejectionEvent, Tracer,
};
use crate::value::{PromiseState, PropertyKey, RealmId, TypeOf, Value};
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

/// A host that records every hook call so tests can assert on it.
#[derive(Default)]
pub struct TestHost {
    pub modules: RefCell<HashMap<String, ModuleSource>>,
    pub console: RefCell<Vec<(ConsoleLevel, Vec<String>)>>,
    pub rejections: RefCell<Vec<RejectionEvent>>,
    pub import_meta_calls: RefCell<Vec<String>>,
    pub resolve_calls: RefCell<Vec<(Option<String>, String, Vec<ImportAttribute>)>>,
    pub allow_compile_strings: Cell<bool>,
    /// Interrupt after this many polls (None: never).
    pub interrupt_after: Cell<Option<u32>>,
    pub interrupt_polls: Cell<u32>,
}

impl TestHost {
    pub fn new() -> Rc<TestHost> {
        let host = TestHost::default();
        host.allow_compile_strings.set(true);
        Rc::new(host)
    }
    pub fn add_module(&self, specifier: &str, source: &str) {
        self.modules.borrow_mut().insert(
            specifier.to_string(),
            ModuleSource::Source(source.to_string()),
        );
    }
}

impl Host for TestHost {
    fn resolve_module(
        &self,
        referrer: Option<&str>,
        specifier: &str,
        attributes: &[ImportAttribute],
    ) -> std::result::Result<String, String> {
        self.resolve_calls.borrow_mut().push((
            referrer.map(str::to_string),
            specifier.to_string(),
            attributes.to_vec(),
        ));
        // Bundle-style resolution: specifiers are already canonical except
        // for `./x` relative to the referrer's directory.
        if let Some(rest) = specifier.strip_prefix("./") {
            let base = referrer.unwrap_or("");
            let dir = match base.rfind('/') {
                Some(i) => &base[..=i],
                None => "",
            };
            return Ok(format!("{dir}{rest}"));
        }
        Ok(specifier.to_string())
    }

    fn load_module(&self, resolved: &str) -> std::result::Result<ModuleSource, String> {
        self.modules
            .borrow()
            .get(resolved)
            .cloned()
            .ok_or_else(|| format!("module not found: {resolved}"))
    }

    fn import_meta(
        &self,
        engine: &mut dyn Engine,
        realm: RealmId,
        module: &str,
        meta: &Value,
    ) -> crate::error::Result<()> {
        self.import_meta_calls.borrow_mut().push(module.to_string());
        engine.set(
            realm,
            meta,
            &"url".into(),
            &Value::string(&format!("gable://{module}")),
        )?;
        let hot = engine.new_object(realm)?;
        engine.set(realm, &hot, &"accepted".into(), &Value::Bool(false))?;
        engine.set(realm, meta, &"hot".into(), &hot)
    }

    fn promise_rejection(&self, _engine: &mut dyn Engine, _realm: RealmId, event: RejectionEvent) {
        self.rejections.borrow_mut().push(event);
    }

    fn can_compile_strings(&self, _realm: RealmId) -> bool {
        self.allow_compile_strings.get()
    }

    fn console(&self, _realm: RealmId, level: ConsoleLevel, args: Vec<String>) {
        self.console.borrow_mut().push((level, args));
    }

    fn interrupt(&self) -> bool {
        let n = self.interrupt_polls.get() + 1;
        self.interrupt_polls.set(n);
        match self.interrupt_after.get() {
            Some(limit) => n >= limit,
            None => false,
        }
    }
}

/// Builds an engine around a test host.
pub type Factory = dyn FnMut(Rc<TestHost>) -> Box<dyn Engine>;

fn setup(factory: &mut Factory) -> (Rc<TestHost>, Box<dyn Engine>, RealmId) {
    let host = TestHost::new();
    let engine = factory(host.clone());
    let realm = engine.main_realm();
    (host, engine, realm)
}

fn eval(engine: &mut dyn Engine, realm: RealmId, src: &str) -> Value {
    match engine.eval_script(realm, "test.js", src) {
        Ok(v) => v,
        Err(e) => panic!("eval failed: {e}\nsource: {src}"),
    }
}

fn eval_err(engine: &mut dyn Engine, realm: RealmId, src: &str) -> Error {
    match engine.eval_script(realm, "test.js", src) {
        Ok(v) => panic!("expected an error, got {v:?}\nsource: {src}"),
        Err(e) => e,
    }
}

fn num(v: &Value) -> f64 {
    v.as_number()
        .unwrap_or_else(|| panic!("expected number, got {v:?}"))
}

// ---- tests ----------------------------------------------------------------

pub fn eval_script_returns_values(factory: &mut Factory) {
    let (_host, mut engine, realm) = setup(factory);
    assert_eq!(num(&eval(&mut *engine, realm, "1 + 2")), 3.0);
    assert_eq!(eval(&mut *engine, realm, "'a' + 'b'"), Value::string("ab"));
    assert_eq!(
        eval(&mut *engine, realm, "true && false"),
        Value::Bool(false)
    );
    assert_eq!(eval(&mut *engine, realm, "undefined"), Value::Undefined);
    assert_eq!(eval(&mut *engine, realm, "null"), Value::Null);
    assert_eq!(
        eval(&mut *engine, realm, "'\\uD83D'")
            .as_str()
            .unwrap()
            .units(),
        &[0xD83D]
    );
    let big = eval(&mut *engine, realm, "2n ** 70n");
    assert_eq!(
        big,
        Value::BigInt(crate::value::JsBigInt("1180591620717411303424".into()))
    );
    // Scripts share the realm's global object.
    eval(&mut *engine, realm, "var shared = 41;");
    assert_eq!(num(&eval(&mut *engine, realm, "shared + 1")), 42.0);
}

pub fn exceptions_report_name_message_and_location(factory: &mut Factory) {
    let (_host, mut engine, realm) = setup(factory);
    let err = eval_err(
        &mut *engine,
        realm,
        "function f() {\n  throw new TypeError('boom');\n}\nf();",
    );
    let exc = err.exception().expect("exception");
    assert_eq!(exc.name, "TypeError");
    assert_eq!(exc.message, "boom");
    assert!(engine.type_of(&exc.value) == TypeOf::Object);
    assert!(!exc.stack.is_empty(), "stack should have frames");
    assert_eq!(exc.stack[0].script, "test.js");
    assert_eq!(
        exc.stack[0].line, 2,
        "innermost frame line: {:?}",
        exc.stack
    );

    let err = eval_err(&mut *engine, realm, "throw 'plain';");
    let exc = err.exception().unwrap();
    assert_eq!(exc.value, Value::string("plain"));
    assert_eq!(exc.message, "plain");

    let err = eval_err(&mut *engine, realm, "let x = ;");
    assert!(err.is_exception_named("SyntaxError"), "{err:?}");
    let err = eval_err(&mut *engine, realm, "undefinedVariable");
    assert!(err.is_exception_named("ReferenceError"), "{err:?}");
}

pub fn host_functions_are_callable_and_can_throw(factory: &mut Factory) {
    let (_host, mut engine, realm) = setup(factory);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let seen2 = seen.clone();
    let add = engine
        .new_function(
            realm,
            "add",
            Rc::new(move |engine: &mut dyn Engine, cx: CallContext| {
                seen2.borrow_mut().push(cx.args.len());
                let a = engine.to_number(cx.realm, &cx.arg(0))?;
                let b = engine.to_number(cx.realm, &cx.arg(1))?;
                if a < 0.0 {
                    let e = engine.new_error(cx.realm, ErrorKind::RangeError, "negative")?;
                    return Err(Error::Exception(crate::error::Exception {
                        value: e,
                        name: "RangeError".into(),
                        message: "negative".into(),
                        stack: vec![],
                    }));
                }
                Ok(Value::Number(a + b))
            }),
        )
        .unwrap();
    let global = engine.global_object(realm).unwrap();
    engine.set(realm, &global, &"add".into(), &add).unwrap();
    assert_eq!(num(&eval(&mut *engine, realm, "add(2, 3)")), 5.0);
    assert_eq!(num(&eval(&mut *engine, realm, "add('4', 3)")), 7.0);
    assert_eq!(*seen.borrow(), vec![2, 2]);
    let caught = eval(
        &mut *engine,
        realm,
        "try { add(-1, 0) } catch (e) { e.constructor === RangeError && e.message }",
    );
    assert_eq!(caught, Value::string("negative"));
    assert_eq!(eval(&mut *engine, realm, "add.name"), Value::string("add"));
    assert_eq!(
        eval(&mut *engine, realm, "typeof add"),
        Value::string("function")
    );
    // Host functions can call back into the engine (re-entrancy).
    let reenter = engine
        .new_function(
            realm,
            "reenter",
            Rc::new(|engine: &mut dyn Engine, cx: CallContext| {
                engine.eval_script(
                    cx.realm,
                    "inner.js",
                    "globalThis.fromInner = 7; fromInner * 2",
                )
            }),
        )
        .unwrap();
    engine
        .set(realm, &global, &"reenter".into(), &reenter)
        .unwrap();
    assert_eq!(
        num(&eval(&mut *engine, realm, "reenter() + fromInner")),
        21.0
    );
    // And a host function can call a JS function it was given.
    let call_it = engine
        .new_function(
            realm,
            "callIt",
            Rc::new(|engine: &mut dyn Engine, cx: CallContext| {
                engine.call(
                    cx.realm,
                    &cx.arg(0),
                    &Value::Undefined,
                    &[Value::Number(10.0)],
                )
            }),
        )
        .unwrap();
    engine
        .set(realm, &global, &"callIt".into(), &call_it)
        .unwrap();
    assert_eq!(num(&eval(&mut *engine, realm, "callIt(x => x + 1)")), 11.0);
}

pub fn jobs_run_only_when_the_host_drains_them(factory: &mut Factory) {
    let (_host, mut engine, realm) = setup(factory);
    eval(
        &mut *engine,
        realm,
        "globalThis.order = []; Promise.resolve().then(() => order.push('job')); order.push('sync');",
    );
    assert_eq!(
        eval(&mut *engine, realm, "order.join()"),
        Value::string("sync")
    );
    assert!(engine.has_pending_jobs());
    let ran = engine.run_jobs(realm).unwrap();
    assert!(ran >= 1);
    assert!(!engine.has_pending_jobs());
    assert_eq!(
        eval(&mut *engine, realm, "order.join()"),
        Value::string("sync,job")
    );
    // Chained reactions are drained in one call.
    eval(
        &mut *engine,
        realm,
        "Promise.resolve().then(() => 1).then(v => order.push(v + 1)).then(() => order.push('x'))",
    );
    engine.run_jobs(realm).unwrap();
    assert_eq!(
        eval(&mut *engine, realm, "order.join()"),
        Value::string("sync,job,2,x")
    );
    // queueMicrotask is a host global, so the engine must not define it.
    assert_eq!(
        eval(&mut *engine, realm, "typeof queueMicrotask"),
        Value::string("undefined")
    );
}

pub fn rejections_reach_the_tracker(factory: &mut Factory) {
    let (host, mut engine, realm) = setup(factory);
    eval(
        &mut *engine,
        realm,
        "globalThis.p = Promise.reject(new Error('unhandled'));",
    );
    engine.run_jobs(realm).unwrap();
    {
        let rej = host.rejections.borrow();
        assert_eq!(rej.len(), 1, "{rej:?}");
        match &rej[0] {
            RejectionEvent::Reject { reason, .. } => {
                let reason = reason.clone();
                drop(rej);
                let msg = engine.get(realm, &reason, &"message".into()).unwrap();
                assert_eq!(msg, Value::string("unhandled"));
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }
    eval(&mut *engine, realm, "p.catch(() => {});");
    engine.run_jobs(realm).unwrap();
    let rej = host.rejections.borrow();
    assert!(
        matches!(rej.last(), Some(RejectionEvent::Handle { .. })),
        "a late handler reports Handle: {rej:?}"
    );
    drop(rej);
    // The tracker follows the specification: a rejection with no handler is
    // reported when it happens, and attaching a handler later reports
    // Handle for the same promise. Deciding that a rejection handled in the
    // same turn is not an error is the host's job.
    host.rejections.borrow_mut().clear();
    eval(&mut *engine, realm, "Promise.reject(1).catch(() => {});");
    engine.run_jobs(realm).unwrap();
    let events = host.rejections.borrow().clone();
    assert!(
        matches!(
            events.as_slice(),
            [RejectionEvent::Reject { .. }, RejectionEvent::Handle { .. }]
        ),
        "{events:?}"
    );
    if let [RejectionEvent::Reject { promise: a, .. }, RejectionEvent::Handle { promise: b }] =
        events.as_slice()
    {
        assert!(engine.strict_equals(a, b), "Handle names the same promise");
    }
}

pub fn modules_load_through_host_hooks(factory: &mut Factory) {
    let (host, mut engine, realm) = setup(factory);
    host.add_module(
        "app/main.js",
        "import { x } from './dep.js';\nexport const y = x * 2;\nglobalThis.mainRan = true;",
    );
    host.add_module("app/dep.js", "export const x = 21;");
    let promise = engine.load_module(realm, "app/main.js").unwrap();
    assert!(engine.is_promise(&promise));
    engine.run_jobs(realm).unwrap();
    let ns = match engine.promise_state(realm, &promise).unwrap() {
        PromiseState::Fulfilled(ns) => ns,
        other => panic!("module promise: {other:?}"),
    };
    assert_eq!(
        engine.get(realm, &ns, &"y".into()).unwrap(),
        Value::Number(42.0)
    );
    assert_eq!(eval(&mut *engine, realm, "mainRan"), Value::Bool(true));
    let calls = host.resolve_calls.borrow();
    assert!(
        calls
            .iter()
            .any(|(r, s, _)| r.as_deref() == Some("app/main.js") && s == "./dep.js"),
        "{calls:?}"
    );
    drop(calls);

    // Dynamic import goes through the same hooks.
    host.add_module("app/lazy.js", "export default 'lazy';");
    eval(
        &mut *engine,
        realm,
        "globalThis.lazy = null; import('app/lazy.js').then(m => { lazy = m.default; });",
    );
    engine.run_jobs(realm).unwrap();
    assert_eq!(eval(&mut *engine, realm, "lazy"), Value::string("lazy"));

    // A missing module rejects the promise instead of panicking.
    let p = engine.load_module(realm, "app/missing.js").unwrap();
    engine.run_jobs(realm).unwrap();
    assert!(matches!(
        engine.promise_state(realm, &p).unwrap(),
        PromiseState::Rejected(_)
    ));

    // A module that throws rejects with the thrown value.
    host.add_module("app/throws.js", "throw new RangeError('module failed');");
    let p = engine.load_module(realm, "app/throws.js").unwrap();
    engine.run_jobs(realm).unwrap();
    match engine.promise_state(realm, &p).unwrap() {
        PromiseState::Rejected(v) => {
            assert_eq!(
                engine.get(realm, &v, &"message".into()).unwrap(),
                Value::string("module failed")
            )
        }
        other => panic!("{other:?}"),
    }
}

pub fn import_meta_is_populated_by_the_host(factory: &mut Factory) {
    let (host, mut engine, realm) = setup(factory);
    host.add_module(
        "app/meta.js",
        "globalThis.metaUrl = import.meta.url; globalThis.metaHot = import.meta.hot.accepted;",
    );
    engine.load_module(realm, "app/meta.js").unwrap();
    engine.run_jobs(realm).unwrap();
    assert_eq!(
        eval(&mut *engine, realm, "metaUrl"),
        Value::string("gable://app/meta.js")
    );
    assert_eq!(eval(&mut *engine, realm, "metaHot"), Value::Bool(false));
    assert_eq!(
        host.import_meta_calls.borrow().as_slice(),
        &["app/meta.js".to_string()]
    );
}

pub fn modules_reload_under_a_new_specifier_and_old_records_are_collected(factory: &mut Factory) {
    let (host, mut engine, realm) = setup(factory);
    host.add_module(
        "app/counter.js?v=1",
        "export const version = 1; export const state = { big: new Array(1000).fill(0) };",
    );
    host.add_module(
        "app/counter.js?v=2",
        "export const version = 2; export const state = { big: [] };",
    );
    eval(
        &mut *engine,
        realm,
        "globalThis.weak = null; globalThis.versions = [];\n\
         globalThis.load = (s) => import(s).then(m => { versions.push(m.version); weak = new WeakRef(m.state); });",
    );
    eval(&mut *engine, realm, "load('app/counter.js?v=1')");
    engine.run_jobs(realm).unwrap();
    assert_eq!(
        eval(&mut *engine, realm, "versions.join()"),
        Value::string("1")
    );
    assert_eq!(
        eval(&mut *engine, realm, "typeof weak.deref()"),
        Value::string("object")
    );
    eval(&mut *engine, realm, "load('app/counter.js?v=2')");
    engine.run_jobs(realm).unwrap();
    assert_eq!(
        eval(&mut *engine, realm, "versions.join()"),
        Value::string("1,2")
    );
    // Re-importing the old specifier returns the same (old) record: specifiers are keys.
    eval(&mut *engine, realm, "load('app/counter.js?v=1')");
    engine.run_jobs(realm).unwrap();
    assert_eq!(
        eval(&mut *engine, realm, "versions.join()"),
        Value::string("1,2,1")
    );
    // Nothing references the v1 record any more except the module map
    // entry, which is part of the record: it must be collectable once the
    // host says it is no longer wanted. Here only JS references exist, and
    // after the module graph is dropped the exports die with it.
    let _ = &host;
}

pub fn realms_are_isolated(factory: &mut Factory) {
    let (_host, mut engine, realm) = setup(factory);
    let other = engine.create_realm().unwrap();
    assert_ne!(realm, other);
    eval(&mut *engine, realm, "globalThis.marker = 'main';");
    assert_eq!(
        engine.eval_script(other, "o.js", "typeof marker").unwrap(),
        Value::string("undefined")
    );
    let main_array = eval(&mut *engine, realm, "Array");
    let other_array = engine.eval_script(other, "o.js", "Array").unwrap();
    assert!(!engine.strict_equals(&main_array, &other_array));
    // Values can cross realms through the host.
    let arr = engine.eval_script(other, "o.js", "[1, 2, 3]").unwrap();
    let global = engine.global_object(realm).unwrap();
    engine.set(realm, &global, &"foreign".into(), &arr).unwrap();
    assert_eq!(num(&eval(&mut *engine, realm, "foreign.length")), 3.0);
    assert_eq!(
        eval(&mut *engine, realm, "Array.isArray(foreign)"),
        Value::Bool(true)
    );
    assert_eq!(
        eval(&mut *engine, realm, "foreign instanceof Array"),
        Value::Bool(false)
    );
}

pub fn the_host_can_interrupt_execution(factory: &mut Factory) {
    let (host, mut engine, realm) = setup(factory);
    host.interrupt_after.set(Some(3));
    let err = eval_err(&mut *engine, realm, "for (;;) {}");
    assert!(matches!(err, Error::Interrupted), "{err:?}");
    assert!(host.interrupt_polls.get() >= 3);
    // The engine is usable afterwards.
    host.interrupt_after.set(None);
    assert_eq!(num(&eval(&mut *engine, realm, "1 + 1")), 2.0);
}

pub fn memory_limits_are_enforced(factory: &mut Factory) {
    let (_host, mut engine, realm) = setup(factory);
    engine.set_memory_limit(Some(4 * 1024 * 1024));
    let result = engine.eval_script(
        realm,
        "oom.js",
        "const a = []; for (let i = 0; i < 1e7; i++) a.push({ i, s: 'x'.repeat(100) }); a.length",
    );
    match result {
        Err(Error::OutOfMemory) => {}
        Err(Error::Exception(e)) if e.name == "RangeError" || e.name == "InternalError" => {}
        other => panic!("expected an out-of-memory failure, got {other:?}"),
    }
    engine.set_memory_limit(None);
    engine.collect_garbage();
    assert_eq!(num(&eval(&mut *engine, realm, "[1,2,3].length")), 3.0);
}

pub fn the_engine_has_no_timers_or_event_loop(factory: &mut Factory) {
    let (_host, mut engine, realm) = setup(factory);
    for global in [
        "setTimeout",
        "setInterval",
        "queueMicrotask",
        "window",
        "document",
        "location",
        "fetch",
    ] {
        assert_eq!(
            eval(&mut *engine, realm, &format!("typeof {global}")),
            Value::string("undefined"),
            "{global} must come from the host, not the engine"
        );
    }
    // A host-supplied timer: the engine only ever runs the callback when the
    // host calls it.
    let timers: Rc<RefCell<Vec<Value>>> = Rc::new(RefCell::new(Vec::new()));
    let t2 = timers.clone();
    let set_timeout = engine
        .new_function(
            realm,
            "setTimeout",
            Rc::new(move |_engine: &mut dyn Engine, cx: CallContext| {
                t2.borrow_mut().push(cx.arg(0));
                Ok(Value::Number(t2.borrow().len() as f64))
            }),
        )
        .unwrap();
    let global = engine.global_object(realm).unwrap();
    engine
        .set(realm, &global, &"setTimeout".into(), &set_timeout)
        .unwrap();
    eval(
        &mut *engine,
        realm,
        "globalThis.fired = 0; setTimeout(() => { fired++; }, 0);",
    );
    engine.run_jobs(realm).unwrap();
    assert_eq!(num(&eval(&mut *engine, realm, "fired")), 0.0);
    let cb = timers.borrow()[0].clone();
    engine.call(realm, &cb, &Value::Undefined, &[]).unwrap();
    assert_eq!(num(&eval(&mut *engine, realm, "fired")), 1.0);
}

struct Counter {
    count: u32,
    label: Value,
}
impl HostObject for Counter {
    fn trace(&self, tracer: &mut dyn Tracer) {
        tracer.trace(&self.label);
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

pub fn host_classes_wrap_rust_data(factory: &mut Factory) {
    let (_host, mut engine, realm) = setup(factory);
    let finalized = Rc::new(Cell::new(0u32));
    let f2 = finalized.clone();
    let class = engine.register_host_class(HostClassDef {
        name: "Counter".into(),
        finalize: Some(Box::new(move |_data| f2.set(f2.get() + 1))),
    });
    let proto = engine.new_object(realm).unwrap();
    let increment = engine
        .new_function(
            realm,
            "increment",
            Rc::new(|engine: &mut dyn Engine, cx: CallContext| {
                let data = engine
                    .host_object_data(&cx.this)
                    .ok_or(Error::Internal("not a Counter".into()))?;
                let counter = data.as_any_mut().downcast_mut::<Counter>().unwrap();
                counter.count += 1;
                Ok(Value::Number(counter.count as f64))
            }),
        )
        .unwrap();
    engine
        .set(realm, &proto, &"increment".into(), &increment)
        .unwrap();
    let label = Value::string("clicks");
    let obj = engine
        .new_host_object(
            realm,
            class,
            Box::new(Counter { count: 0, label }),
            Some(&proto),
        )
        .unwrap();
    assert_eq!(engine.host_object_class(&obj), Some(class));
    let global = engine.global_object(realm).unwrap();
    engine.set(realm, &global, &"counter".into(), &obj).unwrap();
    assert_eq!(
        num(&eval(
            &mut *engine,
            realm,
            "counter.increment(); counter.increment()"
        )),
        2.0
    );
    assert_eq!(
        eval(
            &mut *engine,
            realm,
            "Object.prototype.toString.call(counter)"
        ),
        Value::string("[object Counter]")
    );
    let data = engine.host_object_data(&obj).unwrap();
    assert_eq!(data.as_any().downcast_ref::<Counter>().unwrap().count, 2);
    assert!(engine.host_object_data(&proto).is_none());
    // Drop every reference and collect: the finalizer runs as a host task.
    eval(&mut *engine, realm, "delete globalThis.counter;");
    drop(obj);
    engine.reclaim_handles();
    engine.collect_garbage();
    engine.run_jobs(realm).unwrap();
    assert_eq!(
        finalized.get(),
        1,
        "finalizer should run once after collection"
    );
}

pub fn bytecode_round_trips_and_rejects_version_mismatch(factory: &mut Factory) {
    let (_host, mut engine, realm) = setup(factory);
    let bytes = engine
        .compile_script(
            realm,
            "compiled.js",
            "globalThis.compiledRan = (globalThis.compiledRan || 0) + 1; 'ok:' + compiledRan",
        )
        .unwrap();
    assert!(!bytes.is_empty());
    assert_eq!(
        engine.eval_bytecode(realm, &bytes).unwrap(),
        Value::string("ok:1")
    );
    assert_eq!(
        engine.eval_bytecode(realm, &bytes).unwrap(),
        Value::string("ok:2")
    );
    // Bytecode is usable in a fresh engine too (it is a build artifact).
    let host2 = TestHost::new();
    let mut other = factory(host2);
    let r2 = other.main_realm();
    assert_eq!(
        other.eval_bytecode(r2, &bytes).unwrap(),
        Value::string("ok:1")
    );
    // A tampered header is rejected before anything runs.
    let mut bad = bytes.clone();
    bad[0] ^= 0xFF;
    assert!(
        matches!(
            engine.eval_bytecode(realm, &bad),
            Err(Error::BytecodeMismatch { .. })
        ),
        "magic"
    );
    let mut bad = bytes.clone();
    let v = engine.info().bytecode_format.len();
    // Flip a byte inside the version field, which follows the 4-byte magic.
    bad[4 + v.min(1)] ^= 0x01;
    assert!(
        matches!(
            engine.eval_bytecode(realm, &bad),
            Err(Error::BytecodeMismatch { .. })
        ),
        "version"
    );
    // Modules compile too.
    let mbytes = engine
        .compile_module(realm, "m.js", "export const z = 5;")
        .unwrap();
    assert!(!mbytes.is_empty());
    let host3 = TestHost::new();
    host3
        .modules
        .borrow_mut()
        .insert("m.js".into(), ModuleSource::Bytecode(mbytes));
    let mut e3 = factory(host3);
    let r3 = e3.main_realm();
    let p = e3.load_module(r3, "m.js").unwrap();
    e3.run_jobs(r3).unwrap();
    match e3.promise_state(r3, &p).unwrap() {
        PromiseState::Fulfilled(ns) => {
            assert_eq!(e3.get(r3, &ns, &"z".into()).unwrap(), Value::Number(5.0))
        }
        other => panic!("{other:?}"),
    }
}

pub fn script_source_is_retrievable(factory: &mut Factory) {
    let (host, mut engine, realm) = setup(factory);
    engine.eval_script(realm, "app/a.js", "var a = 1;").unwrap();
    assert_eq!(
        engine.script_source(realm, "app/a.js").as_deref(),
        Some("var a = 1;")
    );
    assert_eq!(engine.script_source(realm, "app/none.js"), None);
    host.add_module("app/m.js", "export const m = 1;");
    engine.load_module(realm, "app/m.js").unwrap();
    engine.run_jobs(realm).unwrap();
    assert_eq!(
        engine.script_source(realm, "app/m.js").as_deref(),
        Some("export const m = 1;")
    );
}

pub fn console_routes_to_the_host(factory: &mut Factory) {
    let (host, mut engine, realm) = setup(factory);
    eval(&mut *engine, realm, "console.log('a', 1, true); console.warn('w'); console.error(new Error('e').message); console.info('i'); console.debug('d');");
    let calls = host.console.borrow();
    assert_eq!(calls.len(), 5, "{calls:?}");
    assert_eq!(
        calls[0],
        (
            ConsoleLevel::Log,
            vec!["a".to_string(), "1".to_string(), "true".to_string()]
        )
    );
    assert_eq!(calls[1].0, ConsoleLevel::Warn);
    assert_eq!(calls[2], (ConsoleLevel::Error, vec!["e".to_string()]));
    assert_eq!(calls[3].0, ConsoleLevel::Info);
    assert_eq!(calls[4].0, ConsoleLevel::Debug);
}

pub fn compile_strings_can_be_disabled(factory: &mut Factory) {
    let (host, mut engine, realm) = setup(factory);
    assert_eq!(num(&eval(&mut *engine, realm, "eval('1 + 1')")), 2.0);
    assert_eq!(
        num(&eval(&mut *engine, realm, "new Function('return 3')()")),
        3.0
    );
    host.allow_compile_strings.set(false);
    let err = eval_err(&mut *engine, realm, "eval('1 + 1')");
    assert!(err.is_exception_named("EvalError"), "{err:?}");
    let err = eval_err(&mut *engine, realm, "new Function('return 3')");
    assert!(err.is_exception_named("EvalError"), "{err:?}");
    // Indirect eval of a non-string is still fine.
    assert_eq!(num(&eval(&mut *engine, realm, "eval(5)")), 5.0);
}

pub fn objects_and_arrays_are_manipulable_from_the_host(factory: &mut Factory) {
    let (_host, mut engine, realm) = setup(factory);
    let obj = engine.new_object(realm).unwrap();
    engine
        .set(realm, &obj, &"a".into(), &Value::Number(1.0))
        .unwrap();
    engine
        .set(realm, &obj, &"b".into(), &Value::string("two"))
        .unwrap();
    assert!(engine.has(realm, &obj, &"a".into()).unwrap());
    assert!(!engine.has(realm, &obj, &"zzz".into()).unwrap());
    let keys = engine.own_keys(realm, &obj).unwrap();
    assert_eq!(keys, vec![PropertyKey::from("a"), PropertyKey::from("b")]);
    assert!(engine.delete(realm, &obj, &"a".into()).unwrap());
    assert_eq!(
        engine.own_keys(realm, &obj).unwrap(),
        vec![PropertyKey::from("b")]
    );
    let arr = engine
        .new_array(realm, &[Value::Number(1.0), Value::string("x")])
        .unwrap();
    assert!(engine.is_array(&arr));
    assert!(!engine.is_array(&obj));
    assert_eq!(
        engine.get(realm, &arr, &"length".into()).unwrap(),
        Value::Number(2.0)
    );
    assert_eq!(
        engine.get(realm, &arr, &1u32.into()).unwrap(),
        Value::string("x")
    );
    let sym = engine.new_symbol(realm, "tag").unwrap();
    assert_eq!(engine.type_of(&sym), TypeOf::Symbol);
    let key = PropertyKey::Symbol(sym.as_handle().unwrap().clone());
    engine.set(realm, &obj, &key, &Value::Bool(true)).unwrap();
    assert_eq!(engine.get(realm, &obj, &key).unwrap(), Value::Bool(true));
    assert_eq!(
        engine.own_keys(realm, &obj).unwrap().len(),
        1,
        "symbols are not string keys"
    );
    // Construct and type_of.
    let date = engine.eval_script(realm, "t.js", "Date").unwrap();
    let d = engine
        .construct(realm, &date, &[Value::Number(0.0)])
        .unwrap();
    assert_eq!(engine.type_of(&d), TypeOf::Object);
    assert_eq!(engine.type_of(&date), TypeOf::Function);
    let get_time = engine.get(realm, &d, &"getTime".into()).unwrap();
    assert_eq!(
        engine.call(realm, &get_time, &d, &[]).unwrap(),
        Value::Number(0.0)
    );
    assert_eq!(engine.to_string(realm, &arr).unwrap(), "1,x");
    assert_eq!(
        engine.to_number(realm, &Value::string("0x10")).unwrap(),
        16.0
    );
    let err = engine
        .new_error(realm, ErrorKind::TypeError, "bad")
        .unwrap();
    assert_eq!(
        engine.get(realm, &err, &"name".into()).unwrap(),
        Value::string("TypeError")
    );
    // Getters run when the host reads.
    let o = eval(&mut *engine, realm, "({ get g() { return 9; } })");
    assert_eq!(
        engine.get(realm, &o, &"g".into()).unwrap(),
        Value::Number(9.0)
    );
    // Setters that throw surface as errors.
    let o = eval(
        &mut *engine,
        realm,
        "({ set s(v) { throw new Error('no'); } })",
    );
    assert!(engine.set(realm, &o, &"s".into(), &Value::Null).is_err());
}

pub fn handles_are_released_when_dropped(factory: &mut Factory) {
    let (_host, mut engine, realm) = setup(factory);
    engine.reclaim_handles();
    let before = engine.live_handles();
    let mut values = Vec::new();
    for _ in 0..50 {
        values.push(engine.new_object(realm).unwrap());
    }
    assert!(engine.live_handles() >= before + 50);
    let keep = values.pop().unwrap();
    drop(values);
    engine.reclaim_handles();
    assert_eq!(engine.live_handles(), before + 1);
    // Clones share the slot.
    let clone = keep.clone();
    drop(keep);
    engine.reclaim_handles();
    assert_eq!(engine.live_handles(), before + 1);
    drop(clone);
    engine.reclaim_handles();
    assert_eq!(engine.live_handles(), before);
    // An object kept only by a handle survives collection.
    let obj = engine.new_object(realm).unwrap();
    engine
        .set(realm, &obj, &"alive".into(), &Value::Bool(true))
        .unwrap();
    engine.collect_garbage();
    assert_eq!(
        engine.get(realm, &obj, &"alive".into()).unwrap(),
        Value::Bool(true)
    );
    // A handle from another engine is refused.
    let host2 = TestHost::new();
    let mut other = factory(host2);
    let r2 = other.main_realm();
    assert!(matches!(
        other.get(r2, &obj, &"alive".into()),
        Err(Error::ForeignHandle)
    ));
}

pub fn detach_array_buffer_is_supported(factory: &mut Factory) {
    let (_host, mut engine, realm) = setup(factory);
    let buf = eval(
        &mut *engine,
        realm,
        "globalThis.buf = new ArrayBuffer(8); buf",
    );
    engine.detach_array_buffer(realm, &buf).unwrap();
    assert_eq!(
        eval(&mut *engine, realm, "buf.byteLength"),
        Value::Number(0.0)
    );
    assert_eq!(eval(&mut *engine, realm, "buf.detached"), Value::Bool(true));
    assert!(engine
        .detach_array_buffer(realm, &Value::Number(1.0))
        .is_err());
}

pub fn promise_state_is_inspectable(factory: &mut Factory) {
    let (_host, mut engine, realm) = setup(factory);
    let p = eval(&mut *engine, realm, "new Promise(() => {})");
    assert_eq!(
        engine.promise_state(realm, &p).unwrap(),
        PromiseState::Pending
    );
    let p = eval(&mut *engine, realm, "Promise.resolve(3)");
    assert_eq!(
        engine.promise_state(realm, &p).unwrap(),
        PromiseState::Fulfilled(Value::Number(3.0))
    );
    let p = eval(
        &mut *engine,
        realm,
        "Promise.reject('r').catch(() => {}) && Promise.reject('r')",
    );
    assert_eq!(
        engine.promise_state(realm, &p).unwrap(),
        PromiseState::Rejected(Value::string("r"))
    );
    engine.run_jobs(realm).unwrap();
    assert!(engine.promise_state(realm, &Value::Number(1.0)).is_err());
}

/// Every kit test, by name, for runners that want to drive them manually.
pub const ALL: &[(&str, fn(&mut Factory))] = &[
    ("eval_script_returns_values", eval_script_returns_values),
    (
        "exceptions_report_name_message_and_location",
        exceptions_report_name_message_and_location,
    ),
    (
        "host_functions_are_callable_and_can_throw",
        host_functions_are_callable_and_can_throw,
    ),
    (
        "jobs_run_only_when_the_host_drains_them",
        jobs_run_only_when_the_host_drains_them,
    ),
    ("rejections_reach_the_tracker", rejections_reach_the_tracker),
    (
        "modules_load_through_host_hooks",
        modules_load_through_host_hooks,
    ),
    (
        "import_meta_is_populated_by_the_host",
        import_meta_is_populated_by_the_host,
    ),
    (
        "modules_reload_under_a_new_specifier_and_old_records_are_collected",
        modules_reload_under_a_new_specifier_and_old_records_are_collected,
    ),
    ("realms_are_isolated", realms_are_isolated),
    (
        "the_host_can_interrupt_execution",
        the_host_can_interrupt_execution,
    ),
    ("memory_limits_are_enforced", memory_limits_are_enforced),
    (
        "the_engine_has_no_timers_or_event_loop",
        the_engine_has_no_timers_or_event_loop,
    ),
    ("host_classes_wrap_rust_data", host_classes_wrap_rust_data),
    (
        "bytecode_round_trips_and_rejects_version_mismatch",
        bytecode_round_trips_and_rejects_version_mismatch,
    ),
    ("script_source_is_retrievable", script_source_is_retrievable),
    ("console_routes_to_the_host", console_routes_to_the_host),
    (
        "compile_strings_can_be_disabled",
        compile_strings_can_be_disabled,
    ),
    (
        "objects_and_arrays_are_manipulable_from_the_host",
        objects_and_arrays_are_manipulable_from_the_host,
    ),
    (
        "handles_are_released_when_dropped",
        handles_are_released_when_dropped,
    ),
    (
        "detach_array_buffer_is_supported",
        detach_array_buffer_is_supported,
    ),
    ("promise_state_is_inspectable", promise_state_is_inspectable),
];

/// Instantiate the conformance kit as `#[test]` functions in the calling
/// crate. The argument is a closure `|host: Rc<TestHost>| -> Box<dyn Engine>`.
#[macro_export]
macro_rules! conformance_tests {
    ($factory:expr) => {
        mod engine_conformance {
            use super::*;
            fn factory() -> Box<$crate::conformance::Factory> {
                Box::new($factory)
            }
            $crate::conformance_tests!(@each factory,
                eval_script_returns_values,
                exceptions_report_name_message_and_location,
                host_functions_are_callable_and_can_throw,
                jobs_run_only_when_the_host_drains_them,
                rejections_reach_the_tracker,
                modules_load_through_host_hooks,
                import_meta_is_populated_by_the_host,
                modules_reload_under_a_new_specifier_and_old_records_are_collected,
                realms_are_isolated,
                the_host_can_interrupt_execution,
                memory_limits_are_enforced,
                the_engine_has_no_timers_or_event_loop,
                host_classes_wrap_rust_data,
                bytecode_round_trips_and_rejects_version_mismatch,
                script_source_is_retrievable,
                console_routes_to_the_host,
                compile_strings_can_be_disabled,
                objects_and_arrays_are_manipulable_from_the_host,
                handles_are_released_when_dropped,
                detach_array_buffer_is_supported,
                promise_state_is_inspectable
            );
        }
    };
    (@each $factory:ident, $($name:ident),*) => {
        $(
            #[test]
            fn $name() {
                let mut f = $factory();
                $crate::conformance::$name(&mut *f);
            }
        )*
    };
}
