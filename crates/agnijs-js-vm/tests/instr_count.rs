//! Instructions dispatched per unit of work on the hot workloads
//! (TASK-100.18, finding 3 of `docs/js-engine-performance-findings.md`).
//! Built with `--features instr-count`, the interpreter counts every
//! instruction it dispatches; these tests pin the counts the compiler's
//! register-operand and fused compare-and-branch forms deliver, so a
//! lowering change that reintroduces accumulator traffic fails here before
//! it shows up in a benchmark.
//!
//! Run with `cargo test -p agnijs-js-vm --features instr-count --test instr_count`.
#![cfg(feature = "instr-count")]

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, RealmId, Value};
use agnijs_js_vm::Vm;

fn engine() -> (Vm, RealmId) {
    let host = TestHost::new();
    let mut vm = Vm::new(host);
    Vm::create_realm(&mut vm);
    let realm = vm.main_realm();
    (vm, realm)
}

fn eval(vm: &mut Vm, realm: RealmId, src: &str) -> Value {
    vm.eval_script(realm, "t.js", src)
        .unwrap_or_else(|err| panic!("{err}: {src}"))
}

/// Instructions dispatched by `call`, minus those dispatched by the same
/// call with the work scaled to `small` (fixed entry and exit cost).
fn count(vm: &mut Vm, realm: RealmId, call: &str) -> u64 {
    let before = vm.instr_count;
    eval(vm, realm, call);
    vm.instr_count - before
}

fn per_unit(vm: &mut Vm, realm: RealmId, f: &str, small: u64, large: u64) -> f64 {
    let a = count(vm, realm, &format!("{f}({small})"));
    let b = count(vm, realm, &format!("{f}({large})"));
    (b - a) as f64 / (large - small) as f64
}

#[test]
fn loop_arith_dispatches_at_most_14_instructions_per_iteration() {
    // 23 before TASK-100.18: every operand went through the accumulator
    // and the loop test was a compare, a store and a branch.
    let (mut vm, realm) = engine();
    eval(
        &mut vm,
        realm,
        "function loopArith(n) {
           var s = 0;
           for (var i = 0; i < n; i++) { s = (s + i * 7) % 1000003; }
           return s;
         }",
    );
    assert_eq!(eval(&mut vm, realm, "loopArith(100)"), Value::Number(34650.0));
    let per_iter = per_unit(&mut vm, realm, "loopArith", 1000, 2000);
    eprintln!("loop-arith: {per_iter} instructions per iteration");
    assert!(per_iter <= 14.0, "loop-arith: {per_iter} instructions per iteration (bar 14, was 23)");
}

#[test]
fn calls_recursive_dispatches_at_most_15_instructions_per_call() {
    // 20 before TASK-100.18, averaged over the base and recursive cases.
    let (mut vm, realm) = engine();
    eval(
        &mut vm,
        realm,
        "function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }
         function calls(n) { return fib(n); }",
    );
    assert_eq!(eval(&mut vm, realm, "fib(20)"), Value::Number(6765.0));
    // fib(n) makes 2*fib(n+1)-1 calls: fib(15) 1973, fib(20) 21891.
    let small = count(&mut vm, realm, "fib(15)");
    let large = count(&mut vm, realm, "fib(20)");
    let per_call = (large - small) as f64 / (21891 - 1973) as f64;
    eprintln!("calls-recursive: {per_call} instructions per call");
    assert!(per_call <= 15.0, "calls-recursive: {per_call} instructions per call (bar 15, was 20)");
}

#[test]
fn prop_monomorphic_inner_loop_is_recorded() {
    // No bar in the ticket (35 before TASK-100.18); the count is printed so
    // the task notes can record it, and pinned loosely against regression.
    let (mut vm, realm) = engine();
    eval(
        &mut vm,
        realm,
        "var pts = [];
         for (var k = 0; k < 100; k++) pts.push({ x: k, y: k * 2 });
         function propMono(rounds) {
           var s = 0;
           for (var r = 0; r < rounds; r++) {
             for (var i = 0; i < pts.length; i++) { var p = pts[i]; s = (s + p.x + p.y) | 0; }
           }
           return s;
         }",
    );
    assert_eq!(eval(&mut vm, realm, "propMono(1)"), Value::Number(14850.0));
    let per_round = per_unit(&mut vm, realm, "propMono", 10, 20);
    let per_iter = per_round / 100.0;
    eprintln!("prop-monomorphic: {per_iter} instructions per inner iteration");
    assert!(per_iter <= 30.0, "prop-monomorphic: {per_iter} per inner iteration (was 35.2)");
}
