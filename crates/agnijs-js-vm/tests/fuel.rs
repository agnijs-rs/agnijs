//! Fuel (TASK-103.1): an instruction budget the host sets on the engine.
//! Every bytecode instruction the engine dispatches costs one unit; when
//! the budget is spent the running call fails with `Error::OutOfFuel`, at
//! the same instruction on every run and platform.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Error, RealmId, Value};
use agnijs_js_vm::Vm;

fn engine() -> (Box<dyn Engine>, RealmId) {
    let vm = Vm::new(TestHost::new());
    let realm = vm.main_realm();
    (Box::new(vm), realm)
}

fn eval(e: &mut dyn Engine, realm: RealmId, src: &str) -> Value {
    e.eval_script(realm, "t.js", src)
        .unwrap_or_else(|err| panic!("{err}: {src}"))
}

#[test]
fn an_endless_loop_runs_out_of_fuel() {
    let (mut e, realm) = engine();
    e.set_fuel(Some(10_000)).unwrap();
    let r = e.eval_script(realm, "spin.js", "for (;;) {}");
    assert!(matches!(r, Err(Error::OutOfFuel)), "{r:?}");
    assert_eq!(e.fuel(), Some(0));
}

/// What `src` costs: the fuel a generous budget loses running it.
fn cost(src: &str) -> u64 {
    let (mut e, realm) = engine();
    e.set_fuel(Some(u64::MAX)).unwrap();
    eval(&mut *e, realm, src);
    u64::MAX - e.fuel().unwrap()
}

const SUM: &str = "let s = 0; for (let i = 0; i < 1000; i++) s += i; s";

#[test]
fn a_budget_of_exactly_the_cost_is_enough_and_one_less_is_not() {
    let n = cost(SUM);
    assert!(n > 1000, "{n}");
    let (mut e, realm) = engine();
    e.set_fuel(Some(n)).unwrap();
    assert_eq!(eval(&mut *e, realm, SUM), Value::Number(499500.0));
    assert_eq!(e.fuel(), Some(0));

    let (mut e, realm) = engine();
    e.set_fuel(Some(n - 1)).unwrap();
    let r = e.eval_script(realm, "t.js", SUM);
    assert!(matches!(r, Err(Error::OutOfFuel)), "{r:?}");
}

#[test]
fn exhaustion_comes_at_the_same_instruction_on_every_run() {
    // The cost of a program is a property of the program and the engine
    // build: pinned here so a change to it is a visible decision (the
    // bytecode changed), and checked by CI on every platform.
    assert_eq!(cost(SUM), 11016);
    let stop = |budget| {
        let (mut e, realm) = engine();
        eval(&mut *e, realm, "var n = 0;");
        e.set_fuel(Some(budget)).unwrap();
        let r = e.eval_script(realm, "t.js", "for (;;) n++;");
        assert!(matches!(r, Err(Error::OutOfFuel)), "{r:?}");
        e.set_fuel(None).unwrap();
        eval(&mut *e, realm, "n")
    };
    let first = stop(100_000);
    for _ in 0..3 {
        assert_eq!(stop(100_000), first);
    }
    assert_eq!(first, Value::Number(12499.0));
}

#[test]
fn javascript_cannot_catch_running_out_of_fuel() {
    let (mut e, realm) = engine();
    eval(&mut *e, realm, "var caught = false, finished = false;");
    e.set_fuel(Some(50_000)).unwrap();
    let r = e.eval_script(
        realm,
        "t.js",
        "try { for (;;) {} } catch (x) { caught = true; } finally { finished = true; }
         for (;;) {}",
    );
    assert!(matches!(r, Err(Error::OutOfFuel)), "{r:?}");
    e.set_fuel(None).unwrap();
    assert_eq!(eval(&mut *e, realm, "caught || finished"), Value::Bool(false));
}

#[test]
fn a_spent_budget_fails_every_call_until_the_host_sets_a_new_one() {
    let (mut e, realm) = engine();
    e.set_fuel(Some(1_000)).unwrap();
    assert!(matches!(e.eval_script(realm, "a.js", "for (;;) {}"), Err(Error::OutOfFuel)));
    assert!(matches!(e.eval_script(realm, "b.js", "1 + 1"), Err(Error::OutOfFuel)));
    e.set_fuel(Some(1_000)).unwrap();
    assert_eq!(eval(&mut *e, realm, "1 + 1"), Value::Number(2.0));
    e.set_fuel(None).unwrap();
    assert_eq!(e.fuel(), None);
    assert_eq!(eval(&mut *e, realm, "let k = 0; for (let i = 0; i < 1e5; i++) k++; k"), Value::Number(1e5));
}

#[test]
fn the_budget_spans_callbacks_from_natives_and_promise_jobs() {
    // Code a native calls back, and code a job runs, costs fuel like any
    // other: neither is a way around the budget.
    for src in [
        "[1, 2, 3].forEach(() => { for (;;) {} })",
        "[3, 1, 2].sort(() => { for (;;) {} })",
        "({ get x() { for (;;) {} } }).x",
    ] {
        let (mut e, realm) = engine();
        e.set_fuel(Some(100_000)).unwrap();
        let r = e.eval_script(realm, "t.js", src);
        assert!(matches!(r, Err(Error::OutOfFuel)), "{src}: {r:?}");
    }
    let (mut e, realm) = engine();
    eval(&mut *e, realm, "Promise.resolve().then(() => { for (;;) {} });");
    e.set_fuel(Some(100_000)).unwrap();
    assert!(matches!(e.run_jobs(realm), Err(Error::OutOfFuel)));
}

#[test]
fn a_host_function_can_set_a_budget_for_the_rest_of_the_call() {
    use agnijs_engine_api::CallContext;
    use std::rc::Rc;
    let (mut e, realm) = engine();
    let f = e
        .new_function(
            realm,
            "limit",
            Rc::new(|e: &mut dyn Engine, _cx: CallContext| {
                e.set_fuel(Some(10_000))?;
                Ok(Value::Undefined)
            }),
        )
        .unwrap();
    let global = e.global_object(realm).unwrap();
    e.set(realm, &global, &"limit".into(), &f).unwrap();
    let r = e.eval_script(realm, "t.js", "limit(); for (;;) {}");
    assert!(matches!(r, Err(Error::OutOfFuel)), "{r:?}");
}

/// Compiled code is never entered while a budget is set, so a program
/// costs the same with the JIT on as with it off.
#[cfg(feature = "jit")]
#[test]
fn the_jit_does_not_change_what_a_program_costs() {
    use agnijs_js_vm::interp::jit::JitMode;
    for mode in [JitMode::Off, JitMode::On, JitMode::Force] {
        let mut vm = Vm::new(TestHost::new());
        vm.set_jit_mode(mode);
        let realm = vm.main_realm();
        let mut e: Box<dyn Engine> = Box::new(vm);
        e.set_fuel(Some(u64::MAX)).unwrap();
        eval(&mut *e, realm, SUM);
        assert_eq!(u64::MAX - e.fuel().unwrap(), 11016, "{mode:?}");
        // And a budget set while compiled code runs takes effect at once.
        e.set_fuel(None).unwrap();
        eval(&mut *e, realm, "function hot() { let k = 0; for (let i = 0; i < 1e6; i++) k += i; return k; } hot(); hot();");
        let f = e
            .new_function(
                realm,
                "limit",
                std::rc::Rc::new(|e: &mut dyn Engine, _cx: agnijs_engine_api::CallContext| {
                    e.set_fuel(Some(1_000))?;
                    Ok(Value::Undefined)
                }),
            )
            .unwrap();
        let global = e.global_object(realm).unwrap();
        e.set(realm, &global, &"limit".into(), &f).unwrap();
        let r = e.eval_script(
            realm,
            "t.js",
            "function spin() { let k = 0; for (let i = 0; ; i++) { if (i === 5000) limit(); k += i; } } spin()",
        );
        assert!(matches!(r, Err(Error::OutOfFuel)), "{mode:?}: {r:?}");
    }
}
