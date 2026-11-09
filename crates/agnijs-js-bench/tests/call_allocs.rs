//! A JavaScript-to-JavaScript call allocates nothing on the Rust heap in
//! the common case (TASK-100.19, acceptance criterion 1). A counting
//! global allocator counts the allocations this thread makes while a
//! script runs; running the same call pattern at two sizes cancels the
//! fixed cost (compiling the script, entering the engine), so what is left
//! is the cost per call.
//!
//! This lives in the benchmark crate because a global allocator needs
//! `unsafe`, which every engine crate forbids.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, RealmId, Value};
use agnijs_js_vm::Vm;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

// SAFETY: forwards every request unchanged to the system allocator; the
// counter is a const-initialised thread local, which never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.with(|c| c.set(c.get() + 1));
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCS.with(|c| c.set(c.get() + 1));
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.with(|c| c.set(c.get() + 1));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocs() -> u64 {
    ALLOCS.with(|c| c.get())
}

fn engine() -> (Vm, RealmId) {
    let mut vm = Vm::new(TestHost::new());
    Vm::create_realm(&mut vm);
    let realm = vm.main_realm();
    (vm, realm)
}

fn eval(vm: &mut Vm, realm: RealmId, src: &str) -> Value {
    vm.eval_script(realm, "allocs.js", src)
        .unwrap_or_else(|e| panic!("{e}: {src}"))
}

/// Allocations made by `run(n)`.
fn count(vm: &mut Vm, realm: RealmId, run: &str, n: u64) -> u64 {
    let src = format!("{run}({n})");
    // Warm the register file, the frame stack and the caches to this size.
    eval(vm, realm, &src);
    let before = allocs();
    eval(vm, realm, &src);
    allocs() - before
}

/// Allocations per call of `run`, which makes `calls_per_n` calls per unit.
fn per_call(setup: &str, run: &str, calls_per_n: f64) -> f64 {
    let (mut vm, realm) = engine();
    eval(&mut vm, realm, setup);
    let small = count(&mut vm, realm, run, 2_000);
    let large = count(&mut vm, realm, run, 20_000);
    large.saturating_sub(small) as f64 / (18_000.0 * calls_per_n)
}

fn assert_no_allocations(what: &str, setup: &str, run: &str, calls_per_n: f64) {
    let p = per_call(setup, run, calls_per_n);
    eprintln!("{what}: {p} allocations per call");
    assert!(p < 0.001, "{what}: {p} Rust heap allocations per call");
}

#[test]
fn a_recursive_call_allocates_nothing() {
    // fib(n) makes 2*fib(n+1)-1 calls: fib(15) 1973, fib(20) 21891.
    let (mut vm, realm) = engine();
    eval(&mut vm, realm, "function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }");
    eval(&mut vm, realm, "fib(20)");
    let before = allocs();
    eval(&mut vm, realm, "fib(15)");
    let small = allocs() - before;
    let before = allocs();
    eval(&mut vm, realm, "fib(20)");
    let large = allocs() - before;
    let p = large.saturating_sub(small) as f64 / (21891.0 - 1973.0);
    eprintln!("fib: {p} allocations per call");
    assert!(p < 0.001, "fib: {p} Rust heap allocations per call");
}

#[test]
fn plain_method_closure_and_native_calls_allocate_nothing() {
    assert_no_allocations(
        "plain call",
        "function add(a, b, c) { return a + b + c; }
         function run(n) { var s = 0; for (var i = 0; i < n; i++) s = add(s, i, 1); return s; }",
        "run",
        1.0,
    );
    assert_no_allocations(
        "method call",
        "var o = { k: 1, m(x) { return x + this.k; } };
         function run(n) { var s = 0; for (var i = 0; i < n; i++) s = o.m(s); return s; }",
        "run",
        1.0,
    );
    assert_no_allocations(
        "closure call",
        "function mk() { var c = 0; return function (d) { c += d; return c; }; }
         var f = mk();
         function run(n) { var s = 0; for (var i = 0; i < n; i++) s = f(1); return s; }",
        "run",
        1.0,
    );
    assert_no_allocations(
        "native call",
        "function run(n) { var s = 0; for (var i = 0; i < n; i++) s += Math.abs(-i) + Math.floor(i / 2); return s; }",
        "run",
        2.0,
    );
}

#[test]
fn call_apply_and_getters_allocate_nothing() {
    assert_no_allocations(
        "f.call",
        "function f(a, b) { return a + b; }
         function run(n) { var s = 0; for (var i = 0; i < n; i++) s = f.call(null, s, 1); return s; }",
        "run",
        1.0,
    );
    assert_no_allocations(
        "f.apply",
        "function f(a, b) { return a + b; }
         var args = [1, 2];
         function run(n) { var s = 0; for (var i = 0; i < n; i++) s += f.apply(null, args); return s; }",
        "run",
        1.0,
    );
    assert_no_allocations(
        "getter",
        "class P { constructor() { this.n = 1; } get d() { return this.n * 2; } }
         var p = new P();
         function run(n) { var s = 0; for (var i = 0; i < n; i++) s += p.d; return s; }",
        "run",
        1.0,
    );
}
