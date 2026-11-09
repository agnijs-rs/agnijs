//! The dispatch loop's attention word and interrupt polls (TASK-100.17,
//! TASK-100.33; ADR 0019 J7). The interpreter keeps `pc` and the
//! accumulator in locals and tests one word per instruction; these tests
//! pin down what that word must still deliver: host interrupts at function
//! entry (not only on loop back-edges), host-requested collections at the
//! next safe point, stress-mode collections, and the memory limit on
//! straight-line code.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{CallContext, Engine, Error, RealmId, Value};
use agnijs_js_vm::Vm;
use std::rc::Rc;

fn engine() -> (Rc<TestHost>, Vm, RealmId) {
    let host = TestHost::new();
    let mut vm = Vm::new(host.clone());
    Vm::create_realm(&mut vm);
    let realm = vm.main_realm();
    (host, vm, realm)
}

fn eval(vm: &mut Vm, realm: RealmId, src: &str) -> Value {
    vm.eval_script(realm, "t.js", src)
        .unwrap_or_else(|err| panic!("{err}: {src}"))
}

fn install(vm: &mut Vm, realm: RealmId, name: &str, f: Rc<agnijs_engine_api::HostFunction>) {
    let func = vm.new_function(realm, name, f).unwrap();
    let global = vm.global_object(realm).unwrap();
    Engine::set(vm, realm, &global, &name.into(), &func).unwrap();
}

// ---- interrupts (TASK-100.33) -------------------------------------------------------------------

#[test]
fn a_recursive_function_with_no_loops_is_interrupted_by_the_host() {
    let (host, mut vm, realm) = engine();
    eval(
        &mut vm,
        realm,
        "function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }",
    );
    // Two host polls: the hook is consulted every 4096 polls of the
    // interpreter, so this needs about 8000 function entries, far fewer
    // than fib(30) makes (1.6 M) and far more than the frame limit.
    host.interrupt_after
        .set(Some(host.interrupt_polls.get() + 2));
    let r = vm.eval_script(realm, "fib.js", "fib(30)");
    assert!(matches!(r, Err(Error::Interrupted)), "{r:?}");
    host.interrupt_after.set(None);
    assert_eq!(eval(&mut vm, realm, "fib(10)"), Value::Number(55.0));
}

#[test]
fn a_chain_of_calls_returning_without_loops_is_interrupted_by_the_host() {
    // No loop anywhere: each call returns at once, and the polls come from
    // the entries and the returns alone.
    let (host, mut vm, realm) = engine();
    eval(
        &mut vm,
        realm,
        "function leaf() { return 1; }
         function spin(n) { return n === 0 ? 0 : leaf() + leaf() + spin(n - 1); }",
    );
    host.interrupt_after
        .set(Some(host.interrupt_polls.get() + 2));
    let r = vm.eval_script(realm, "spin.js", "spin(15000)");
    assert!(matches!(r, Err(Error::Interrupted)), "{r:?}");
    host.interrupt_after.set(None);
}

// ---- collections ----------------------------------------------------------------------------------

#[test]
fn a_collection_requested_by_a_host_function_runs_at_the_next_safe_point() {
    let (_host, mut vm, realm) = engine();
    install(
        &mut vm,
        realm,
        "requestGc",
        Rc::new(|engine: &mut dyn Engine, _cx: CallContext| {
            // Frames are on the stack, so this can only request.
            engine.collect_garbage();
            Ok(Value::Undefined)
        }),
    );
    let before = vm.gc_count;
    eval(
        &mut vm,
        realm,
        "var junk = []; for (var i = 0; i < 1000; i++) junk.push({ i });
         junk = null;
         requestGc();
         var s = 0; for (var i = 0; i < 100; i++) s += i;",
    );
    assert!(vm.gc_count > before, "the requested collection never ran");
}

#[test]
fn stress_mode_switched_on_after_creation_collects_at_safe_points() {
    let (_host, mut vm, realm) = engine();
    eval(&mut vm, realm, "function work() { var s = 0; for (var i = 0; i < 300; i++) s += i; return s; }");
    let before = vm.gc_count;
    vm.heap.stress = true;
    assert_eq!(eval(&mut vm, realm, "work()"), Value::Number(44850.0));
    vm.heap.stress = false;
    assert!(
        vm.gc_count - before > 300,
        "only {} collections for a 300-iteration loop under stress",
        vm.gc_count - before
    );
}

#[test]
fn the_memory_limit_stops_straight_line_allocation() {
    let (_host, mut vm, realm) = engine();
    let used = vm.memory_usage().heap_bytes;
    vm.set_memory_limit(Some(used + 1024 * 1024));
    // No loop: each statement doubles the string (flat copies, which the
    // allocator counts; a rope would not be), so the limit must be noticed
    // between instructions. 64 KiB doubled eight times is 16 MiB, past the
    // 8 MiB default collection threshold as well.
    let r = vm.eval_script(
        realm,
        "doubling.js",
        "var a = 'x'.repeat(1 << 16);
         var b = a.repeat(2); var c = b.repeat(2); var d = c.repeat(2); var e = d.repeat(2);
         var f = e.repeat(2); var g = f.repeat(2); var h = g.repeat(2); var i = h.repeat(2);
         i.length",
    );
    assert!(
        matches!(r, Err(Error::OutOfMemory)),
        "{r:?}"
    );
    vm.set_memory_limit(None);
    assert_eq!(eval(&mut vm, realm, "1 + 1"), Value::Number(2.0));
}

// ---- the reproduction -------------------------------------------------------------------------

/// The reproduction from the ticket: an empty counting loop in a module
/// (129 ms before the task against 37 ms on QuickJS-NG). Run with
/// `cargo test --release -p agnijs-js-vm --test dispatch -- --ignored --nocapture`.
#[test]
#[ignore = "timing; run in release mode by hand"]
fn reproduction_module_loop_timing() {
    let mut best = f64::INFINITY;
    // A module body runs once per engine, so each repetition gets a fresh one.
    for _ in 0..7 {
        let (host, mut vm, realm) = engine();
        host.add_module(
            "app/loop.js",
            "const t0 = Date.now(); let s = 0;
             for (let i = 0; i < 1e6; i++) s += 1;
             globalThis.elapsed = Date.now() - t0; globalThis.s = s;",
        );
        vm.load_module(realm, "app/loop.js").unwrap();
        vm.run_jobs(realm).unwrap();
        let elapsed = eval(&mut vm, realm, "elapsed").as_number().unwrap();
        best = best.min(elapsed);
        assert_eq!(eval(&mut vm, realm, "s"), Value::Number(1e6));
    }
    eprintln!("reproduction: 1e6-iteration module loop, best of 7: {best} ms");
}
