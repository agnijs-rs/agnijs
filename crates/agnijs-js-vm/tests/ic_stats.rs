//! Inline-cache hit rates on the access patterns TASK-100.20 targets.
//! Built with `--features ic-stats`, the VM counts hits, misses and fills;
//! these tests pin that the steady state of each pattern is all hits, so a
//! change that quietly stops caching one fails here before it shows up in a
//! benchmark.
//!
//! Run with `cargo test -p agnijs-js-vm --features ic-stats --test ic_stats -- --nocapture`
//! to see the counts.
#![cfg(feature = "ic-stats")]

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::Engine;
use agnijs_js_vm::Vm;

#[derive(Debug, Default, Clone, Copy)]
struct Counts {
    get_hits: u64,
    get_misses: u64,
    set_hits: u64,
    set_misses: u64,
    keyed_hits: u64,
    keyed_misses: u64,
    megamorphic: u64,
}

fn snapshot(vm: &Vm) -> Counts {
    let s = &vm.ic_stats;
    Counts {
        get_hits: s.get_hits.get(),
        get_misses: s.get_misses.get(),
        set_hits: s.set_hits.get(),
        set_misses: s.set_misses.get(),
        keyed_hits: s.keyed_hits.get(),
        keyed_misses: s.keyed_misses.get(),
        megamorphic: s.megamorphic.get(),
    }
}

/// Counts for `run` after `setup` and one warm-up call of `run`.
fn measure(name: &str, setup: &str, run: &str) -> Counts {
    let mut vm = Vm::new(TestHost::new());
    let realm = vm.main_realm();
    vm.eval_script(realm, "setup.js", setup).unwrap();
    vm.eval_script(realm, "warm.js", run).unwrap();
    let a = snapshot(&vm);
    vm.eval_script(realm, "run.js", run).unwrap();
    let b = snapshot(&vm);
    let d = Counts {
        get_hits: b.get_hits - a.get_hits,
        get_misses: b.get_misses - a.get_misses,
        set_hits: b.set_hits - a.set_hits,
        set_misses: b.set_misses - a.set_misses,
        keyed_hits: b.keyed_hits - a.keyed_hits,
        keyed_misses: b.keyed_misses - a.keyed_misses,
        megamorphic: b.megamorphic - a.megamorphic,
    };
    println!("{name:>18}: {d:?}");
    d
}

/// In the steady state no named or keyed access misses (a handful of
/// misses from the `run` script's own top level are allowed).
fn assert_all_hits(c: Counts, named: u64, keyed: u64) {
    assert!(c.get_hits + c.set_hits >= named, "{c:?}");
    assert!(c.get_misses + c.set_misses <= 4, "{c:?}");
    assert!(c.keyed_hits >= keyed, "{c:?}");
    assert!(c.keyed_misses <= 4, "{c:?}");
    assert_eq!(c.megamorphic, 0, "{c:?}");
}

#[test]
fn four_shapes_at_one_site() {
    let c = measure(
        "four-shapes",
        "var four = [{ a: 1, b: 1 }, { b: 1, a: 2 }, { a: 3, b: 1, c: 2 }, { c: 2, b: 1, a: 4 }];
         function f() { var s = 0; for (var i = 0; i < 1000; i++) s = (s + four[i & 3].a) | 0; return s; }",
        "f()",
    );
    assert_all_hits(c, 1000, 1000);
}

#[test]
fn array_length_and_elements() {
    let c = measure(
        "array",
        "var data = []; for (var i = 0; i < 100; i++) data.push(i);
         function f() { var s = 0; for (var i = 0; i < data.length; i++) { s = (s + data[i]) | 0; data[i] = s; } return s; }",
        "f()",
    );
    assert_all_hits(c, 100, 200);
}

#[test]
fn string_length_and_methods() {
    let c = measure(
        "string",
        "var strs = ['abc', 'defg', 'hi', 'jklmn'];
         function f() { var s = 0; for (var i = 0; i < 1000; i++) s = (s + strs[i & 3].length + strs[i & 3].charCodeAt(0)) | 0; return s; }",
        "f()",
    );
    assert_all_hits(c, 2000, 2000);
}

#[test]
fn getters_setters_and_two_class_methods() {
    let c = measure(
        "method-calls",
        "class Shape { constructor(n) { this.n = n; } area() { return this.n; } get double() { return this.n * 2; } set half(v) { this.n = v * 2; } }
         class Square extends Shape { area() { return super.area() * this.n; } }
         var shapes = []; for (var i = 0; i < 100; i++) shapes.push(i % 2 ? new Shape(i) : new Square(i));
         function f() { var s = 0; for (var i = 0; i < shapes.length; i++) { var x = shapes[i]; s = (s + x.area() + x.double) | 0; x.half = x.n / 2; } return s; }",
        "f()",
    );
    // shapes.length, x.area, x.double, x.half, this.n (several).
    assert_all_hits(c, 500, 100);
}
