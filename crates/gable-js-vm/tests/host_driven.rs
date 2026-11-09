//! The host drives the engine: it decides when jobs run, supplies timers,
//! bounds execution with interrupts and memory limits, and creates realms.
//! These go beyond the conformance kit's smoke checks, through the engine
//! interface only.

use gable_engine_api::conformance::TestHost;
use gable_engine_api::{CallContext, Engine, Error, PromiseState, RealmId, Value};
use std::cell::RefCell;
use std::rc::Rc;

fn engine() -> (Rc<TestHost>, Box<dyn Engine>, RealmId) {
    let host = TestHost::new();
    let e = gable_js_vm::make_engine(host.clone());
    let realm = e.main_realm();
    (host, e, realm)
}

fn eval(e: &mut dyn Engine, realm: RealmId, src: &str) -> Value {
    e.eval_script(realm, "t.js", src)
        .unwrap_or_else(|err| panic!("{err}: {src}"))
}

fn install(e: &mut dyn Engine, realm: RealmId, name: &str, f: Rc<gable_engine_api::HostFunction>) {
    let func = e.new_function(realm, name, f).unwrap();
    let global = e.global_object(realm).unwrap();
    e.set(realm, &global, &name.into(), &func).unwrap();
}

// ---- jobs ---------------------------------------------------------------------------------------

#[test]
fn jobs_wait_for_the_host_and_drain_to_empty_in_one_checkpoint() {
    let (_host, mut e, realm) = engine();
    eval(
        &mut *e,
        realm,
        "globalThis.log = [];
         (async () => { log.push('a1'); await null; log.push('a2'); await null; log.push('a3'); })();
         Promise.resolve().then(() => { log.push('p1'); queue(); });
         function queue() { Promise.resolve().then(() => log.push('p2')); }
         log.push('sync');",
    );
    assert_eq!(eval(&mut *e, realm, "log.join()"), Value::string("a1,sync"));
    assert!(e.has_pending_jobs());
    let ran = e.run_jobs(realm).unwrap();
    assert!(ran >= 4, "ran {ran}");
    assert!(!e.has_pending_jobs());
    assert_eq!(
        eval(&mut *e, realm, "log.join()"),
        Value::string("a1,sync,a2,p1,a3,p2")
    );
    // Nothing left: a second checkpoint runs nothing.
    assert_eq!(e.run_jobs(realm).unwrap(), 0);
}

#[test]
fn a_job_from_another_realm_runs_in_that_realm() {
    let (_host, mut e, main) = engine();
    let other = e.create_realm().unwrap();
    e.eval_script(
        other,
        "o.js",
        "globalThis.seen = null; Promise.resolve().then(() => { seen = [] instanceof Array && typeof marker; });",
    )
    .unwrap();
    eval(&mut *e, main, "globalThis.marker = 'main';");
    // The host's checkpoint runs every queued job, whichever realm it names.
    e.run_jobs(main).unwrap();
    assert_eq!(
        e.eval_script(other, "o.js", "seen").unwrap(),
        Value::string("undefined"),
        "the job saw its own realm's globals and intrinsics"
    );
}

#[test]
fn a_host_function_can_enqueue_work_that_runs_only_at_the_next_checkpoint() {
    let (_host, mut e, realm) = engine();
    let pending: Rc<RefCell<Vec<Value>>> = Rc::default();
    let p2 = pending.clone();
    install(
        &mut *e,
        realm,
        "later",
        Rc::new(move |engine: &mut dyn Engine, cx: CallContext| {
            // Resolve a promise from the host side: its reactions are jobs.
            let p = engine.eval_script(cx.realm, "later.js", "Promise.resolve(7)")?;
            p2.borrow_mut().push(p.clone());
            Ok(p)
        }),
    );
    eval(
        &mut *e,
        realm,
        "globalThis.got = 0; later().then(v => { got = v; });",
    );
    assert_eq!(eval(&mut *e, realm, "got"), Value::Number(0.0));
    e.run_jobs(realm).unwrap();
    assert_eq!(eval(&mut *e, realm, "got"), Value::Number(7.0));
    assert_eq!(pending.borrow().len(), 1);
}

// ---- timers -------------------------------------------------------------------------------------

/// A host event loop with a virtual clock: `setTimeout` and `clearTimeout`
/// are host functions; each fired timer is a host task followed by a
/// microtask checkpoint.
struct Timers {
    now: f64,
    next_id: u32,
    queue: Vec<(f64, u32, Value)>,
}

fn install_timers(e: &mut dyn Engine, realm: RealmId) -> Rc<RefCell<Timers>> {
    let timers = Rc::new(RefCell::new(Timers {
        now: 0.0,
        next_id: 1,
        queue: Vec::new(),
    }));
    let t = timers.clone();
    install(
        e,
        realm,
        "setTimeout",
        Rc::new(move |engine: &mut dyn Engine, cx: CallContext| {
            let delay = engine
                .to_number(cx.realm, &cx.arg(1))
                .unwrap_or(0.0)
                .max(0.0);
            let mut t = t.borrow_mut();
            let id = t.next_id;
            t.next_id += 1;
            let at = t.now + if delay.is_nan() { 0.0 } else { delay };
            t.queue.push((at, id, cx.arg(0)));
            Ok(Value::Number(id as f64))
        }),
    );
    let t = timers.clone();
    install(
        e,
        realm,
        "clearTimeout",
        Rc::new(move |engine: &mut dyn Engine, cx: CallContext| {
            let id = engine.to_number(cx.realm, &cx.arg(0))? as u32;
            t.borrow_mut().queue.retain(|(_, i, _)| *i != id);
            Ok(Value::Undefined)
        }),
    );
    timers
}

/// Advance the virtual clock to `until`, firing due timers in order.
fn advance(e: &mut dyn Engine, realm: RealmId, timers: &Rc<RefCell<Timers>>, until: f64) -> usize {
    let mut fired = 0;
    loop {
        let next = {
            let mut t = timers.borrow_mut();
            t.queue
                .sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap().then(a.1.cmp(&b.1)));
            match t.queue.first() {
                Some((at, _, _)) if *at <= until => {
                    let (at, _, cb) = t.queue.remove(0);
                    t.now = at;
                    Some(cb)
                }
                _ => None,
            }
        };
        let Some(cb) = next else { break };
        e.call(realm, &cb, &Value::Undefined, &[]).unwrap();
        e.run_jobs(realm).unwrap();
        fired += 1;
    }
    timers.borrow_mut().now = until;
    fired
}

#[test]
fn host_timers_fire_in_due_order_with_a_checkpoint_after_each() {
    let (_host, mut e, realm) = engine();
    let timers = install_timers(&mut *e, realm);
    eval(
        &mut *e,
        realm,
        "globalThis.log = [];
         setTimeout(() => log.push('t20'), 20);
         setTimeout(() => { log.push('t10'); Promise.resolve().then(() => log.push('m10')); }, 10);
         const cancelled = setTimeout(() => log.push('never'), 15);
         setTimeout(() => { log.push('t0'); setTimeout(() => log.push('t0+5'), 5); }, 0);
         clearTimeout(cancelled);
         Promise.resolve().then(() => log.push('m-sync'));",
    );
    e.run_jobs(realm).unwrap();
    assert_eq!(eval(&mut *e, realm, "log.join()"), Value::string("m-sync"));
    assert_eq!(advance(&mut *e, realm, &timers, 9.0), 2);
    assert_eq!(
        eval(&mut *e, realm, "log.join()"),
        Value::string("m-sync,t0,t0+5")
    );
    assert_eq!(advance(&mut *e, realm, &timers, 100.0), 2);
    assert_eq!(
        eval(&mut *e, realm, "log.join()"),
        Value::string("m-sync,t0,t0+5,t10,m10,t20")
    );
    assert!(timers.borrow().queue.is_empty());
}

#[test]
fn an_awaited_host_timer_resumes_an_async_function() {
    let (_host, mut e, realm) = engine();
    let timers = install_timers(&mut *e, realm);
    eval(
        &mut *e,
        realm,
        "globalThis.steps = [];
         const sleep = (ms) => new Promise(r => setTimeout(r, ms));
         globalThis.done = (async () => { steps.push(0); await sleep(50); steps.push(50); await sleep(50); steps.push(100); return 'ok'; })();",
    );
    e.run_jobs(realm).unwrap();
    let done = eval(&mut *e, realm, "done");
    assert_eq!(
        e.promise_state(realm, &done).unwrap(),
        PromiseState::Pending
    );
    advance(&mut *e, realm, &timers, 60.0);
    assert_eq!(eval(&mut *e, realm, "steps.join()"), Value::string("0,50"));
    advance(&mut *e, realm, &timers, 200.0);
    assert_eq!(
        e.promise_state(realm, &done).unwrap(),
        PromiseState::Fulfilled(Value::string("ok"))
    );
}

// ---- interrupts ---------------------------------------------------------------------------------

#[test]
fn an_interrupt_stops_a_runaway_job_and_the_engine_carries_on() {
    let (host, mut e, realm) = engine();
    eval(
        &mut *e,
        realm,
        "globalThis.after = false;
         Promise.resolve().then(() => { for (;;) {} });
         Promise.resolve().then(() => { after = true; });",
    );
    host.interrupt_after
        .set(Some(host.interrupt_polls.get() + 5));
    assert!(matches!(e.run_jobs(realm), Err(Error::Interrupted)));
    host.interrupt_after.set(None);
    // The interrupted job is gone; the queue continues with the next one.
    e.run_jobs(realm).unwrap();
    assert_eq!(eval(&mut *e, realm, "after"), Value::Bool(true));
}

#[test]
fn an_interrupt_unwinds_through_host_functions_and_skips_finally_blocks() {
    let (host, mut e, realm) = engine();
    install(
        &mut *e,
        realm,
        "callBack",
        Rc::new(|engine: &mut dyn Engine, cx: CallContext| {
            engine.call(cx.realm, &cx.arg(0), &Value::Undefined, &[])
        }),
    );
    host.interrupt_after
        .set(Some(host.interrupt_polls.get() + 5));
    let r = e.eval_script(
        realm,
        "spin.js",
        "globalThis.cleaned = false;
         try { callBack(() => { try { for (;;) {} } catch (e) { globalThis.caught = e; } }); }
         finally { cleaned = true; }",
    );
    assert!(matches!(r, Err(Error::Interrupted)), "{r:?}");
    host.interrupt_after.set(None);
    assert_eq!(
        eval(&mut *e, realm, "typeof caught"),
        Value::string("undefined"),
        "JS cannot catch an interrupt"
    );
    assert_eq!(eval(&mut *e, realm, "cleaned"), Value::Bool(false));
}

#[test]
fn an_interrupt_during_module_evaluation_is_reported_to_the_host() {
    let (host, mut e, realm) = engine();
    host.add_module("app/spin.js", "for (;;) {}");
    host.interrupt_after
        .set(Some(host.interrupt_polls.get() + 5));
    let r = e.load_module(realm, "app/spin.js");
    let r = match r {
        Ok(_) => e.run_jobs(realm).map(|_| ()),
        Err(err) => Err(err),
    };
    assert!(matches!(r, Err(Error::Interrupted)), "{r:?}");
    host.interrupt_after.set(None);
    assert_eq!(eval(&mut *e, realm, "6 * 7"), Value::Number(42.0));
}

// ---- memory limits ------------------------------------------------------------------------------

fn is_oom(r: &Result<impl std::fmt::Debug, Error>) -> bool {
    match r {
        Err(Error::OutOfMemory) => true,
        Err(Error::Exception(e)) => e.name == "RangeError" || e.name == "InternalError",
        _ => false,
    }
}

#[test]
fn memory_usage_is_reported_and_falls_after_collection() {
    let (_host, mut e, realm) = engine();
    e.collect_garbage();
    let base = e.memory_usage();
    eval(
        &mut *e,
        realm,
        "globalThis.big = []; for (let i = 0; i < 20000; i++) big.push({ i });",
    );
    let grown = e.memory_usage();
    assert!(grown.heap_bytes > base.heap_bytes, "{base:?} -> {grown:?}");
    assert!(grown.object_count >= base.object_count + 20000);
    eval(&mut *e, realm, "big = null;");
    e.collect_garbage();
    let after = e.memory_usage();
    assert!(after.object_count < grown.object_count - 19000, "{after:?}");
}

#[test]
fn the_memory_limit_applies_inside_jobs_and_can_be_raised_again() {
    let (_host, mut e, realm) = engine();
    eval(
        &mut *e,
        realm,
        "globalThis.hoard = [];
         Promise.resolve().then(() => { for (;;) hoard.push({ s: 'x'.repeat(64) }); });",
    );
    let used = e.memory_usage().heap_bytes;
    e.set_memory_limit(Some(used + 2 * 1024 * 1024));
    let r = e.run_jobs(realm);
    // Either the job's OOM escapes the checkpoint, or it rejected the job's
    // promise; both stop the job.
    assert!(r.is_err() == is_oom(&r), "{r:?}");
    e.set_memory_limit(None);
    eval(&mut *e, realm, "hoard = null;");
    e.collect_garbage();
    assert_eq!(
        eval(
            &mut *e,
            realm,
            "Array.from({ length: 1000 }, (_, i) => i).length"
        ),
        Value::Number(1000.0)
    );
}

#[test]
fn a_memory_limit_stops_a_huge_string_and_a_runaway_script() {
    // The limit is enforced at the interpreter's safe points against what
    // the allocator has counted (objects, strings, environments, bigints).
    let (_host, mut e, realm) = engine();
    let used = e.memory_usage().heap_bytes;
    e.set_memory_limit(Some(used + 1024 * 1024));
    let r = e.eval_script(realm, "huge.js", "const s = 'x'.repeat(1 << 26); s.length");
    assert!(is_oom(&r), "{r:?}");
    let r = e.eval_script(
        realm,
        "runaway.js",
        "const keep = []; for (let i = 0; ; i++) keep.push({ i, s: 'y' + i });",
    );
    assert!(is_oom(&r), "{r:?}");
    e.set_memory_limit(None);
    assert_eq!(eval(&mut *e, realm, "1 + 1"), Value::Number(2.0));
}

// ---- realms -------------------------------------------------------------------------------------

#[test]
fn a_host_function_can_create_and_populate_a_realm_while_js_runs() {
    let (_host, mut e, main) = engine();
    install(
        &mut *e,
        main,
        "makeRealm",
        Rc::new(|engine: &mut dyn Engine, _cx: CallContext| {
            let r = engine.create_realm()?;
            engine.eval_script(
                r,
                "realm.js",
                "globalThis.kind = 'child'; ({ Array, kind, eval })",
            )
        }),
    );
    let v = eval(
        &mut *e,
        main,
        "const a = makeRealm(), b = makeRealm();
         [a.kind, a.Array !== Array, a.Array !== b.Array, Array.isArray(new a.Array()),
          a.eval('typeof makeRealm'), typeof kind].join()",
    );
    assert_eq!(v, Value::string("child,true,true,true,undefined,undefined"));
}

#[test]
fn each_realm_has_its_own_module_map_and_runs_bytecode() {
    let (host, mut e, main) = engine();
    let other = e.create_realm().unwrap();
    host.add_module(
        "app/count.js",
        "globalThis.loads = (globalThis.loads || 0) + 1; export const n = 1;",
    );
    for realm in [main, other, main] {
        e.load_module(realm, "app/count.js").unwrap();
        e.run_jobs(realm).unwrap();
    }
    assert_eq!(eval(&mut *e, main, "loads"), Value::Number(1.0));
    assert_eq!(
        e.eval_script(other, "o.js", "loads").unwrap(),
        Value::Number(1.0)
    );

    let bytes = e
        .compile_script(
            main,
            "shared.js",
            "globalThis.where = typeof loads; [].constructor === Array",
        )
        .unwrap();
    let third = e.create_realm().unwrap();
    assert_eq!(e.eval_bytecode(third, &bytes).unwrap(), Value::Bool(true));
    assert_eq!(
        e.eval_script(third, "o.js", "where").unwrap(),
        Value::string("undefined")
    );
}

#[test]
fn an_unknown_realm_is_refused() {
    let (_host, mut e, _main) = engine();
    let bogus = RealmId(9999);
    assert!(matches!(
        e.eval_script(bogus, "x.js", "1"),
        Err(Error::UnknownRealm)
    ));
    assert!(matches!(e.run_jobs(bogus), Err(Error::UnknownRealm)));
    assert!(matches!(e.global_object(bogus), Err(Error::UnknownRealm)));
}
