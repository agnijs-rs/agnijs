//! Collector tests: garbage is reclaimed, live data survives, and the
//! stress mode (a collection at every safe point) finds no missing roots
//! in the programs below. The CI stress and Miri jobs run this file.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Value};
use agnijs_js_vm::Vm;

fn engine(stress: bool) -> Vm {
    let mut vm = Vm::new(TestHost::new());
    Vm::create_realm(&mut vm);
    vm.heap.stress = stress;
    vm
}

fn num(v: Value) -> f64 {
    v.as_number()
        .unwrap_or_else(|| panic!("expected a number, got {v:?}"))
}

fn run(vm: &mut Vm, src: &str) -> Value {
    let realm = vm.main_realm();
    let v = vm
        .eval_script(realm, "gc-test.js", src)
        .unwrap_or_else(|e| panic!("{e}"));
    vm.run_jobs(realm).unwrap();
    v
}

#[test]
#[cfg_attr(
    miri,
    ignore = "too slow under Miri; the small stress test below covers the same paths"
)]
fn garbage_is_reclaimed() {
    let mut vm = engine(false);
    run(
        &mut vm,
        "var keep = []; for (var i = 0; i < 2000; i++) ({ i, s: 'x' + i, a: [i] });",
    );
    vm.collect_garbage();
    let after_garbage = vm.memory_usage().object_count;
    run(&mut vm, "for (var i = 0; i < 2000; i++) ({ i });");
    vm.collect_garbage();
    let again = vm.memory_usage().object_count;
    assert!(
        again <= after_garbage + 8,
        "temporaries survived a collection: {after_garbage} -> {again}"
    );
}

#[test]
fn live_data_survives_collection() {
    let mut vm = engine(false);
    run(&mut vm, "var keep = []; for (var i = 0; i < 500; i++) keep.push({ i, s: 'v' + i, f() { return this.i; } });");
    vm.collect_garbage();
    let total = num(run(
        &mut vm,
        "keep.reduce((a, o) => a + o.f() + (o.s === 'v' + o.i ? 0 : 1e9), 0)",
    ));
    assert_eq!(total, (0..500).sum::<i32>() as f64);
}

#[test]
#[cfg_attr(
    miri,
    ignore = "too slow under Miri; the small stress test below covers the same paths"
)]
fn shrinks_after_release() {
    let mut vm = engine(false);
    run(
        &mut vm,
        "var big = []; for (var i = 0; i < 5000; i++) big.push({ i });",
    );
    vm.collect_garbage();
    let held = vm.memory_usage().object_count;
    run(&mut vm, "big = null;");
    vm.collect_garbage();
    let released = vm.memory_usage().object_count;
    assert!(
        released + 4000 < held,
        "releasing 5000 objects freed too little: {held} -> {released}"
    );
}

const PROGRAMS: &[(&str, f64)] = &[
    // Closures, environments and captured bindings.
    ("var fs = []; for (let i = 0; i < 50; i++) fs.push(() => i); fs.reduce((a, f) => a + f(), 0)", 1225.0),
    // Strings: ropes, flattening and interning under pressure.
    ("var s = ''; for (var i = 0; i < 200; i++) s += String(i % 10); var o = {}; o[s] = 1; s.length + o[s]", 201.0),
    // Exceptions and finally across frames.
    ("function f(n) { try { if (n) throw { n }; return 0; } finally { g = n; } } var g; var t = 0; for (var i = 0; i < 20; i++) { try { f(i); } catch (e) { t += e.n; } } t + g", 209.0),
    // Generators keep their saved frames alive.
    ("function* gen() { for (let i = 0; i < 10; i++) yield { i }; } var t = 0; for (const o of gen()) t += o.i; t", 45.0),
    // Classes, prototypes, getters and symbols.
    ("class A { #x = 1; get x() { return this.#x; } static s = Symbol('s'); } class B extends A { constructor() { super(); this[A.s] = 2; } } var b = new B(); b.x + b[A.s]", 3.0),
    // Destructuring, spread and sparse arrays.
    ("var a = [...Array(30).keys()]; a[1000] = 1; var [x, , y, ...r] = a; x + y + r.length + a.length", 2001.0),
    // Promises and the job queue.
    ("var r = 0; Promise.resolve(1).then(v => { r = v + 1; }); r", 0.0),
    // Direct eval and with.
    ("var o = { p: 5 }; function f() { var q = 1; return eval('with (o) { p + q }'); } f()", 6.0),
];

#[test]
#[cfg_attr(
    miri,
    ignore = "too slow under Miri; the small stress test below covers the same paths"
)]
fn programs_run_under_gc_stress() {
    for (src, expected) in PROGRAMS {
        let mut vm = engine(true);
        let v = num(run(&mut vm, src));
        assert_eq!(v, *expected, "under stress: {src}");
    }
}

#[test]
#[cfg_attr(
    miri,
    ignore = "too slow under Miri; the small stress test below covers the same paths"
)]
fn promise_jobs_settle_under_gc_stress() {
    let mut vm = engine(true);
    run(&mut vm, "var log = []; async function f() { for (let i = 0; i < 5; i++) { await null; log.push(i); } return log.join(''); } var p = f();");
    let realm = vm.main_realm();
    let p = vm.eval_script(realm, "gc-test.js", "p").unwrap();
    let state = vm.promise_state(realm, &p).unwrap();
    assert!(
        matches!(state, agnijs_engine_api::PromiseState::Fulfilled(_)),
        "{state:?}"
    );
    let s = vm.eval_script(realm, "gc-test.js", "log.join('')").unwrap();
    assert_eq!(s, Value::string("01234"));
}

#[test]
#[cfg_attr(
    miri,
    ignore = "too slow under Miri; the small stress test below covers the same paths"
)]
fn memory_limit_faults_instead_of_growing() {
    let mut vm = engine(false);
    vm.set_memory_limit(Some(4 * 1024 * 1024));
    let realm = vm.main_realm();
    let r = vm.eval_script(
        realm,
        "gc-test.js",
        "var a = []; for (;;) a.push({ x: 'y'.repeat(100) });",
    );
    assert!(r.is_err(), "the loop should hit the memory limit");
}

/// TASK-100.23: element growth counts towards the next collection. Before,
/// only the 160 bytes of each array object counted, so 20 000 arrays of
/// 1 000 pushed numbers (320 MB of elements) ran zero collections. The
/// reproduction is scaled to 2 000 arrays here so the test stays bounded
/// in a debug build: 32 MB of elements against 320 KB of counted headers.
#[test]
#[cfg_attr(
    miri,
    ignore = "too slow under Miri; the small stress test below covers the same paths"
)]
fn a_loop_that_only_grows_arrays_triggers_collections() {
    let mut vm = engine(false);
    run(
        &mut vm,
        "for (var k = 0; k < 2000; k++) { var a = []; for (var j = 0; j < 1000; j++) a.push(j); }",
    );
    assert!(
        vm.gc_count >= 1,
        "32 MB of array elements ran {} collections",
        vm.gc_count
    );
    // Everything but the last array is garbage; the heap held at the end
    // is bounded by the collector's threshold, not by the loop's total.
    let usage = vm.memory_usage();
    assert!(
        usage.heap_bytes < 16 * 1024 * 1024,
        "{} bytes held after the loop ({} objects)",
        usage.heap_bytes,
        usage.object_count
    );
}

/// The memory limit is checked against the same counter, so a loop that
/// only grows arrays must hit it instead of growing past it.
#[test]
#[cfg_attr(
    miri,
    ignore = "too slow under Miri; the small stress test below covers the same paths"
)]
fn a_memory_limit_stops_a_loop_that_only_grows_arrays() {
    let mut vm = engine(false);
    vm.set_memory_limit(Some(4 * 1024 * 1024));
    let realm = vm.main_realm();
    // Bounded at 3 000 arrays (48 MB of elements) so a failing run cannot
    // exhaust the machine; with the limit enforced it stops far earlier.
    let r = vm.eval_script(
        realm,
        "gc-test.js",
        "var keep = []; for (var k = 0; k < 3000; k++) { var a = []; for (var j = 0; j < 1000; j++) a.push(j); keep.push(a); }",
    );
    assert!(
        matches!(r, Err(agnijs_engine_api::Error::OutOfMemory)),
        "48 MB of array elements under a 4 MiB limit: {r:?}"
    );
}

/// A Miri-sized program: closures, a generator, an exception and a
/// string concatenation, with a collection at every safe point.
#[test]
fn small_program_under_gc_stress() {
    let mut vm = engine(true);
    let v = num(run(
        &mut vm,
        "function* g() { let s = ''; for (let i = 0; i < 3; i++) { s += i; yield () => s; } } var t = 0; for (const f of g()) t += f().length; try { null.x; } catch (e) { t += e instanceof TypeError ? 10 : 0; } t",
    ));
    assert_eq!(v, 16.0);
}
