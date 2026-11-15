//! The host function and host class contract (TASK-104.3): argument and
//! return conversion, how each error a host function returns reaches
//! JavaScript, what keeps values held by host objects alive, when
//! finalizers run, and what a host function may call back into.

use agnijs::api::{
    CallContext, Engine, Error, ErrorKind, Exception, Host, HostClassDef, HostClassId, HostObject,
    PropertyKey, RealmId, Tracer, Value,
};
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

struct Quiet;
impl Host for Quiet {}

fn engine() -> (Box<dyn Engine>, RealmId) {
    let engine = agnijs::new_engine(Rc::new(Quiet));
    let realm = engine.main_realm();
    (engine, realm)
}

/// Bind `f` to the global `name`.
fn define(
    engine: &mut dyn Engine,
    realm: RealmId,
    name: &str,
    f: impl Fn(&mut dyn Engine, CallContext) -> agnijs::api::Result<Value> + 'static,
) {
    let func = engine.new_function(realm, name, Rc::new(f)).unwrap();
    let global = engine.global_object(realm).unwrap();
    engine
        .set(realm, &global, &PropertyKey::from(name), &func)
        .unwrap();
}

fn eval(engine: &mut dyn Engine, realm: RealmId, src: &str) -> Value {
    engine
        .eval_script(realm, "test.js", src)
        .unwrap_or_else(|e| panic!("{src}: {e}"))
}

fn eval_str(engine: &mut dyn Engine, realm: RealmId, src: &str) -> String {
    let v = eval(engine, realm, src);
    engine.to_string(realm, &v).unwrap()
}

// ---- arguments and returns ------------------------------------------------

#[test]
fn arguments_arrive_as_values_and_missing_ones_read_as_undefined() {
    let (mut engine, realm) = engine();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let log = seen.clone();
    define(&mut *engine, realm, "probe", move |engine, cx| {
        let mut out = log.borrow_mut();
        out.push(format!("argc={}", cx.args.len()));
        for i in 0..3 {
            out.push(format!("{:?}", engine.type_of(&cx.arg(i))));
        }
        Ok(Value::Undefined)
    });
    eval(&mut *engine, realm, "probe(1, 'two')");
    assert_eq!(
        *seen.borrow(),
        ["argc=2", "Number", "String", "Undefined"]
    );
}

#[test]
fn this_is_passed_as_is_and_host_functions_are_not_constructors() {
    let (mut engine, realm) = engine();
    define(&mut *engine, realm, "who", |_engine, cx| {
        assert!(cx.new_target.is_none());
        Ok(cx.this)
    });
    assert_eq!(eval(&mut *engine, realm, "who()"), Value::Undefined);
    assert_eq!(
        eval_str(&mut *engine, realm, "const o = { who, tag: 'o' }; o.who().tag"),
        "o"
    );
    assert_eq!(
        eval_str(&mut *engine, realm, "typeof who.call(5)"),
        "number",
        "a primitive this is not boxed"
    );
    assert_eq!(
        eval_str(
            &mut *engine,
            realm,
            "try { new who(); 'constructed' } catch (e) { e.name }"
        ),
        "TypeError"
    );
}

#[test]
fn the_returned_value_is_what_javascript_sees() {
    let (mut engine, realm) = engine();
    define(&mut *engine, realm, "make", |engine, cx| {
        let o = engine.new_object(cx.realm)?;
        engine.set(cx.realm, &o, &PropertyKey::from("n"), &Value::Number(7.0))?;
        Ok(o)
    });
    assert_eq!(
        eval(&mut *engine, realm, "make().n + make().n"),
        Value::Number(14.0)
    );
}

#[test]
fn argument_handles_stay_valid_after_the_call_when_kept() {
    let (mut engine, realm) = engine();
    let kept = Rc::new(RefCell::new(None));
    let slot = kept.clone();
    define(&mut *engine, realm, "keep", move |_engine, cx| {
        *slot.borrow_mut() = Some(cx.arg(0));
        Ok(Value::Undefined)
    });
    eval(&mut *engine, realm, "keep({ label: 'kept' })");
    engine.collect_garbage();
    let v = kept.borrow_mut().take().unwrap();
    let label = engine.get(realm, &v, &PropertyKey::from("label")).unwrap();
    assert_eq!(label, Value::string("kept"));
}

#[test]
fn the_realm_is_the_realm_the_function_was_created_in() {
    let (mut engine, main) = engine();
    let other = engine.create_realm().unwrap();
    let seen = Rc::new(Cell::new(None));
    let s = seen.clone();
    let f = engine
        .new_function(
            main,
            "where",
            Rc::new(move |_engine: &mut dyn Engine, cx: CallContext| {
                s.set(Some(cx.realm));
                Ok(Value::Undefined)
            }),
        )
        .unwrap();
    let global = engine.global_object(other).unwrap();
    engine
        .set(other, &global, &PropertyKey::from("where"), &f)
        .unwrap();
    eval(&mut *engine, other, "where()");
    assert_eq!(seen.get(), Some(main));
}

// ---- errors ---------------------------------------------------------------

#[test]
fn an_exception_error_throws_its_value_unchanged() {
    let (mut engine, realm) = engine();
    define(&mut *engine, realm, "fail", |engine, cx| {
        let e = engine.new_error(cx.realm, ErrorKind::RangeError, "out of range")?;
        Err(Error::Exception(Exception {
            value: e,
            name: "RangeError".into(),
            message: "out of range".into(),
            stack: Vec::new(),
        }))
    });
    define(&mut *engine, realm, "throw42", |_engine, _cx| {
        Err(Error::Exception(Exception {
            value: Value::Number(42.0),
            name: "number".into(),
            message: "42".into(),
            stack: Vec::new(),
        }))
    });
    assert_eq!(
        eval_str(
            &mut *engine,
            realm,
            "try { fail() } catch (e) { `${e instanceof RangeError}:${e.message}` }"
        ),
        "true:out of range"
    );
    assert_eq!(
        eval(&mut *engine, realm, "try { throw42() } catch (e) { e }"),
        Value::Number(42.0)
    );
    // Uncaught, it reaches the host as an exception like any other.
    let err = engine.eval_script(realm, "t.js", "fail()").unwrap_err();
    assert!(err.is_exception_named("RangeError"));
}

#[test]
fn host_and_internal_errors_throw_a_catchable_error() {
    let (mut engine, realm) = engine();
    define(&mut *engine, realm, "hostFail", |_engine, _cx| {
        Err(Error::Host("disk unplugged".into()))
    });
    define(&mut *engine, realm, "internalFail", |_engine, _cx| {
        Err(Error::Internal("bad state".into()))
    });
    assert_eq!(
        eval_str(
            &mut *engine,
            realm,
            "try { hostFail() } catch (e) { `${e.name}:${e.message}` }"
        ),
        "Error:disk unplugged"
    );
    assert_eq!(
        eval_str(
            &mut *engine,
            realm,
            "try { internalFail() } catch (e) { `${e.name}:${e.message}` }"
        ),
        "Error:bad state"
    );
}

#[test]
fn misuse_errors_throw_a_type_error() {
    let (mut engine, realm) = engine();
    let (mut other, other_realm) = self::engine();
    let foreign = other.new_object(other_realm).unwrap();
    define(&mut *engine, realm, "foreignArg", move |engine, cx| {
        engine.get(cx.realm, &foreign, &PropertyKey::from("x"))
    });
    define(&mut *engine, realm, "badRealm", |engine, _cx| {
        engine.new_object(RealmId(9_999))
    });
    for f in ["foreignArg", "badRealm"] {
        assert_eq!(
            eval_str(
                &mut *engine,
                realm,
                &format!("try {{ {f}() }} catch (e) {{ e.name }}")
            ),
            "TypeError",
            "{f}"
        );
    }
}

#[test]
fn returning_a_foreign_handle_throws_a_type_error() {
    let (mut engine, realm) = engine();
    let (mut other, other_realm) = self::engine();
    let foreign = other.new_object(other_realm).unwrap();
    define(&mut *engine, realm, "leak", move |_engine, _cx| {
        Ok(foreign.clone())
    });
    assert_eq!(
        eval_str(&mut *engine, realm, "try { leak() } catch (e) { e.name }"),
        "TypeError"
    );
}

#[test]
fn interrupted_out_of_memory_and_out_of_fuel_cannot_be_caught() {
    for (name, error) in [
        ("interrupt", Error::Interrupted),
        ("oom", Error::OutOfMemory),
        ("fuel", Error::OutOfFuel),
    ] {
        let (mut engine, realm) = engine();
        let reached = Rc::new(Cell::new(false));
        let r = reached.clone();
        define(&mut *engine, realm, "stop", move |_engine, _cx| {
            Err(error.clone())
        });
        define(&mut *engine, realm, "after", move |_engine, _cx| {
            r.set(true);
            Ok(Value::Undefined)
        });
        let err = engine
            .eval_script(
                realm,
                "t.js",
                "try { stop() } catch (e) { after() } finally { after() }",
            )
            .unwrap_err();
        let expected = matches!(
            (&err, name),
            (Error::Interrupted, "interrupt")
                | (Error::OutOfMemory, "oom")
                | (Error::OutOfFuel, "fuel")
        );
        assert!(expected, "{name}: {err:?}");
        assert!(!reached.get(), "{name}: no catch or finally ran");
        // The engine is usable afterwards.
        assert_eq!(eval(&mut *engine, realm, "1 + 1"), Value::Number(2.0));
    }
}

// ---- re-entrancy ----------------------------------------------------------

#[test]
fn a_host_function_may_call_back_into_the_engine() {
    let (mut engine, realm) = engine();
    define(&mut *engine, realm, "twice", |engine, cx| {
        // Call a JavaScript function it was given.
        let f = cx.arg(0);
        let once = engine.call(cx.realm, &f, &Value::Undefined, &[Value::Number(1.0)])?;
        let twice = engine.call(cx.realm, &f, &Value::Undefined, &[once])?;
        Ok(twice)
    });
    define(&mut *engine, realm, "evalInside", |engine, cx| {
        engine.eval_script(cx.realm, "inner.js", "outer * 2")
    });
    assert_eq!(
        eval(&mut *engine, realm, "twice(x => x + 10)"),
        Value::Number(21.0)
    );
    assert_eq!(
        eval(&mut *engine, realm, "var outer = 21; evalInside()"),
        Value::Number(42.0)
    );
    // Nested host functions, each re-entering.
    assert_eq!(
        eval(&mut *engine, realm, "twice(x => twice(y => y * 2) + x)"),
        Value::Number(9.0)
    );
}

#[test]
fn an_exception_crosses_nested_host_frames() {
    let (mut engine, realm) = engine();
    define(&mut *engine, realm, "callIt", |engine, cx| {
        let f = cx.arg(0);
        engine.call(cx.realm, &f, &Value::Undefined, &[])
    });
    // Thrown in JavaScript, through the host function (which propagates
    // the Err with `?`), caught in JavaScript again.
    assert_eq!(
        eval_str(
            &mut *engine,
            realm,
            "try { callIt(() => { throw new SyntaxError('deep') }) } catch (e) { `${e.name}:${e.message}` }"
        ),
        "SyntaxError:deep"
    );
    // A host function may also catch it and carry on.
    define(&mut *engine, realm, "swallow", |engine, cx| {
        let f = cx.arg(0);
        match engine.call(cx.realm, &f, &Value::Undefined, &[]) {
            Err(Error::Exception(e)) => Ok(Value::string(&e.message)),
            other => other,
        }
    });
    assert_eq!(
        eval_str(&mut *engine, realm, "swallow(() => { throw new Error('caught in Rust') })"),
        "caught in Rust"
    );
}

#[test]
fn run_jobs_from_inside_a_host_function_drains_the_queue() {
    let (mut engine, realm) = engine();
    define(&mut *engine, realm, "drain", |engine, cx| {
        let n = engine.run_jobs(cx.realm)?;
        Ok(Value::Number(n as f64))
    });
    let v = eval_str(
        &mut *engine,
        realm,
        "let log = []; Promise.resolve().then(() => log.push('job')); \
         log.push('before'); const n = drain(); log.push('after:' + n); log.join()",
    );
    assert_eq!(v, "before,job,after:1");
    assert!(!engine.has_pending_jobs());
}

// ---- host objects ---------------------------------------------------------

struct Holder {
    held: Option<Value>,
    dropped: Rc<Cell<usize>>,
}
impl HostObject for Holder {
    fn trace(&self, tracer: &mut dyn Tracer) {
        if let Some(v) = &self.held {
            tracer.trace(v);
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
impl Drop for Holder {
    fn drop(&mut self) {
        self.dropped.set(self.dropped.get() + 1);
    }
}

struct Classes {
    holder: HostClassId,
    finalized: Rc<Cell<usize>>,
    dropped: Rc<Cell<usize>>,
}

fn holder_class(engine: &mut dyn Engine) -> Classes {
    let finalized = Rc::new(Cell::new(0));
    let f = finalized.clone();
    let holder = engine.register_host_class(HostClassDef {
        name: "Holder".into(),
        finalize: Some(Box::new(move |data: Box<dyn HostObject>| {
            assert!(data.as_any().is::<Holder>());
            f.set(f.get() + 1);
        })),
    });
    Classes {
        holder,
        finalized,
        dropped: Rc::new(Cell::new(0)),
    }
}

fn collect(engine: &mut dyn Engine, realm: RealmId) {
    engine.collect_garbage();
    engine.run_jobs(realm).unwrap();
}

#[test]
fn host_data_is_reachable_by_class_and_type() {
    let (mut engine, realm) = engine();
    let c = holder_class(&mut *engine);
    let obj = engine
        .new_host_object(
            realm,
            c.holder,
            Box::new(Holder {
                held: None,
                dropped: c.dropped.clone(),
            }),
            None,
        )
        .unwrap();
    assert_eq!(engine.host_object_class(&obj), Some(c.holder));
    let data = engine.host_object_data(&obj).unwrap();
    assert!(data.as_any_mut().downcast_mut::<Holder>().is_some());
    let plain = engine.new_object(realm).unwrap();
    assert!(engine.host_object_data(&plain).is_none());
    assert_eq!(engine.host_object_class(&plain), None);
}

#[test]
fn a_value_held_by_a_host_object_lives_until_the_host_object_is_finalized() {
    let (mut engine, realm) = engine();
    let c = holder_class(&mut *engine);
    let weak = eval(&mut *engine, realm, "globalThis.w = new WeakRef(globalThis.t = {})");
    drop(weak);
    let held = eval(&mut *engine, realm, "t");
    let holder = engine
        .new_host_object(
            realm,
            c.holder,
            Box::new(Holder {
                held: Some(held),
                dropped: c.dropped.clone(),
            }),
            None,
        )
        .unwrap();
    eval(&mut *engine, realm, "t = null");
    collect(&mut *engine, realm);
    // The handle inside the host data is a root of its own.
    assert_eq!(eval_str(&mut *engine, realm, "typeof w.deref()"), "object");
    assert_eq!(c.finalized.get(), 0);

    drop(holder);
    collect(&mut *engine, realm);
    assert_eq!(c.finalized.get(), 1, "the holder was finalized");
    assert_eq!(c.dropped.get(), 1, "its data was dropped after the finalizer");
    // The held handle went with the data, so the next collection frees it.
    collect(&mut *engine, realm);
    assert_eq!(eval_str(&mut *engine, realm, "typeof w.deref()"), "undefined");
}

#[test]
fn a_cycle_through_host_data_is_not_collected_while_the_engine_lives() {
    let (mut engine, realm) = engine();
    let c = holder_class(&mut *engine);
    let obj = engine
        .new_host_object(
            realm,
            c.holder,
            Box::new(Holder {
                held: None,
                dropped: c.dropped.clone(),
            }),
            None,
        )
        .unwrap();
    // The host object holds a handle to itself. AgniJS does not use
    // `HostObject::trace` to turn that handle into an edge, so it stays a
    // root and the object is never found dead.
    let me = obj.clone();
    engine
        .host_object_data(&obj)
        .unwrap()
        .as_any_mut()
        .downcast_mut::<Holder>()
        .unwrap()
        .held = Some(me);
    drop(obj);
    for _ in 0..3 {
        collect(&mut *engine, realm);
    }
    assert_eq!(c.finalized.get(), 0);
    // Dropping the engine drops the data without running the finalizer.
    drop(engine);
    assert_eq!(c.finalized.get(), 0);
    assert_eq!(c.dropped.get(), 1);
}

#[test]
fn finalizers_run_from_run_jobs_not_during_collection() {
    let (mut engine, realm) = engine();
    let c = holder_class(&mut *engine);
    let obj = engine
        .new_host_object(
            realm,
            c.holder,
            Box::new(Holder {
                held: None,
                dropped: c.dropped.clone(),
            }),
            None,
        )
        .unwrap();
    drop(obj);
    engine.collect_garbage();
    assert_eq!(c.finalized.get(), 0, "queued, not run");
    assert!(engine.has_pending_jobs());
    engine.run_jobs(realm).unwrap();
    assert_eq!(c.finalized.get(), 1);
}

#[test]
fn host_objects_take_the_prototype_given() {
    let (mut engine, realm) = engine();
    let c = holder_class(&mut *engine);
    let proto = eval(
        &mut *engine,
        realm,
        "({ describe() { return 'holder:' + Object.keys(this).length } })",
    );
    let obj = engine
        .new_host_object(
            realm,
            c.holder,
            Box::new(Holder {
                held: None,
                dropped: c.dropped.clone(),
            }),
            Some(&proto),
        )
        .unwrap();
    let global = engine.global_object(realm).unwrap();
    engine
        .set(realm, &global, &PropertyKey::from("h"), &obj)
        .unwrap();
    assert_eq!(eval_str(&mut *engine, realm, "h.describe()"), "holder:0");
}
