//! Rust heap allocations per built-in call (TASK-100.29): the string,
//! regex and JSON built-ins allocate the blocks their results need and
//! little else. A counting global allocator counts the allocations this
//! thread makes while a script runs; running the same loop at two sizes
//! cancels the fixed cost, so what is left is the cost per call.
//!
//! Each bound is the measured count after TASK-100.29 with a little
//! headroom; the comment beside it gives the count before. A result string
//! or object costs one block for its storage (plus the elements of an
//! array), so most bounds are the number of values the call returns.
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
    // This counts the built-ins' allocations; a collection per safe point
    // (AGNIJS_GC_STRESS) would count the collector's instead.
    vm.heap.stress = false;
    let realm = vm.main_realm();
    (vm, realm)
}

fn eval(vm: &mut Vm, realm: RealmId, src: &str) -> Value {
    vm.eval_script(realm, "allocs.js", src)
        .unwrap_or_else(|e| panic!("{e}: {src}"))
}

/// Allocations made by `run(n)`.
fn count(vm: &mut Vm, realm: RealmId, n: u64) -> u64 {
    let src = format!("run({n})");
    // Warm the register file, the frame stack and the caches to this size.
    eval(vm, realm, &src);
    let before = allocs();
    eval(vm, realm, &src);
    allocs() - before
}

/// Allocations per evaluation of `expr` (which may use `i`, the loop
/// counter) after `setup`. The loop keeps each result alive in `keep`
/// only until the next iteration, so nothing accumulates.
fn per_call(setup: &str, expr: &str) -> f64 {
    let (mut vm, realm) = engine();
    eval(&mut vm, realm, setup);
    eval(
        &mut vm,
        realm,
        &format!("var keep; function run(n) {{ for (var i = 0; i < n; i++) keep = {expr}; return 0; }}"),
    );
    let small = count(&mut vm, realm, 200);
    let large = count(&mut vm, realm, 2_200);
    large.saturating_sub(small) as f64 / 2_000.0
}

fn check(what: &str, setup: &str, expr: &str, bound: f64) {
    let p = per_call(setup, expr);
    eprintln!("{what:<40} {p:>8.2} allocations per call (bound {bound})");
    // A fraction of an allocation is a table that grew once during the
    // loop, not a cost of every call.
    assert!(p <= bound + 0.25, "{what}: {p} Rust heap allocations per call, bound {bound}");
}

const LINE: &str = "var line = 'user42@example3.com 2026-11-17 id=546';";
const WORDS: &str =
    "var words = []; for (var w = 0; w < 20; w++) words.push('Word' + w); var text = words.join(' ');";
const DOC: &str = "var doc = { id: 7, name: 'leaf7', tags: ['a', 'b'], ok: false, v: 1.75, meta: { depth: 2, label: 'node7' } };
     var json = JSON.stringify(doc);";

#[test]
fn regex_built_ins() {
    // Before: 13.
    check(
        "exec, two captures",
        &format!("{LINE} var re = /^([a-z]+\\d+)@([a-z]+\\d)\\.com/;"),
        "re.exec(line)",
        8.0,
    );
    // Before: 4.
    check(
        "exec, no match",
        &format!("{LINE} var re = /zz(\\d)/;"),
        "re.exec(line)",
        2.0,
    );
    // Before: 8.
    check("test", &format!("{LINE} var re = /id=\\d+$/;"), "re.test(line)", 3.0);
    // Before: 85.
    check(
        "replace, global, 5 matches",
        &format!("{LINE} var re = /\\d+/g;"),
        "line.replace(re, '#')",
        24.0,
    );
    // Before: 36.
    check(
        "replace, $1 template",
        &format!("{LINE} var re = /(\\d+)-(\\d+)/;"),
        "line.replace(re, '$2/$1')",
        13.0,
    );
    // Before: 63.
    check(
        "match, global, 5 matches",
        &format!("{LINE} var re = /\\d+/g;"),
        "line.match(re)",
        28.0,
    );
    // Before: 254.
    check(
        "split by regex, 3 parts",
        &format!("{LINE} var re = /\\s+/;"),
        "line.split(re)",
        170.0,
    );
}

#[test]
fn string_built_ins() {
    // Before: 46.
    check("split(' '), 20 parts", WORDS, "text.split(' ')", 26.0);
    // Before: 3.
    check("toLowerCase", WORDS, "text.toLowerCase()", 1.0);
    // Before: 3.
    check("toUpperCase", WORDS, "text.toUpperCase()", 1.0);
    // Before: 2.
    check("slice", WORDS, "text.slice(10, 40)", 1.0);
    // Before: 1.
    check("indexOf", WORDS, "text.indexOf('Word17')", 0.0);
    // Before: 6.
    check("replace(string, string)", WORDS, "text.replace('Word5', 'X')", 2.0);
    // Before: 8.
    check("join('-'), 20 parts", WORDS, "words.join('-')", 8.0);
    // Before: 2.
    check("trim", "var s = '  padded text  ';", "s.trim()", 1.0);
}

#[test]
fn json_built_ins() {
    // Before: 34.
    check("JSON.parse, 9 values", DOC, "JSON.parse(json)", 9.0);
    // Before: 50.
    check("JSON.stringify, 9 values", DOC, "JSON.stringify(doc)", 10.0);
    // Before: 5.47.
    check("JSON.stringify, number", "", "JSON.stringify(i / 4)", 2.0);
}

#[test]
fn collection_and_object_built_ins() {
    // Before: 0.
    check(
        "Map get + set",
        "var m = new Map(); for (var k = 0; k < 100; k++) m.set(k, k);",
        "m.set(i % 100, m.get(i % 100))",
        0.0,
    );
    // Before: 0.
    check(
        "Set has + add",
        "var s = new Set(); for (var k = 0; k < 100; k++) s.add(k);",
        "s.has(i % 100) && s.add(i % 100)",
        0.0,
    );
    // Before: 3.
    check("Object.keys, 6 keys", DOC, "Object.keys(doc)", 2.0);
    // Before: 9.
    check("Object.entries, 6 keys", DOC, "Object.entries(doc)", 8.0);
}
