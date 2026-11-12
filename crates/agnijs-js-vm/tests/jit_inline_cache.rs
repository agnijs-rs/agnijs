//! Compiled code runs inline-cache hits itself (TASK-101.3.2): named loads
//! and stores that hit a cache entry, dense element reads and writes and
//! array lengths do not call out of machine code, and every case the cache
//! does not cover takes the generic path with the interpreter's answer.
//!
//! Each program runs interpreted, forced (every block compiled on first
//! use) and tiered; the answers must agree. `calls_out` counts the
//! instructions compiled code handed to the runtime.
//!
//! Run with `cargo test -p agnijs-js-vm --features jit --test jit_inline_cache`.
#![cfg(feature = "jit")]

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::Engine;
use agnijs_js_vm::interp::jit::{JitMode, JitStats};
use agnijs_js_vm::Vm;

fn engine(mode: JitMode) -> Vm {
    let mut vm = Vm::new(TestHost::new());
    Vm::create_realm(&mut vm);
    vm.set_jit_mode(mode);
    vm
}

fn run_in(vm: &mut Vm, src: &str) -> String {
    let realm = vm.main_realm();
    let r = vm.eval_script(realm, "t.js", src).and_then(|v| {
        vm.run_jobs(realm)?;
        Engine::to_string(vm, realm, &v)
    });
    match r {
        Ok(s) => s.to_string(),
        Err(e) => format!("error: {e}"),
    }
}

fn run(mode: JitMode, src: &str) -> (String, JitStats) {
    let mut vm = engine(mode);
    let out = run_in(&mut vm, src);
    (out, vm.jit_stats())
}

/// `src` answers `expected` interpreted, forced and tiered; returns the
/// forced run's statistics.
fn same(src: &str, expected: &str) -> JitStats {
    let (off, _) = run(JitMode::Off, src);
    assert_eq!(off, expected, "interpreter: {src}");
    let (forced, stats) = run(JitMode::Force, src);
    assert_eq!(forced, off, "forced JIT vs interpreter: {src}");
    let (tiered, _) = run(JitMode::On, src);
    assert_eq!(tiered, off, "tiered JIT vs interpreter: {src}");
    stats
}

/// The loop in each of these runs 10000 times; compiled code that called
/// out for its property accesses would call out at least that often.
const N: u64 = 10_000;

fn mostly_inline(stats: JitStats) {
    assert!(stats.compiled >= 1, "{stats:?}");
    // Under AGNIJS_GC_STRESS every back edge is a safe point that collects,
    // so compiled code polls (a call out) once per iteration by design; the
    // values are still checked above, the bound only holds without stress.
    if std::env::var_os("AGNIJS_GC_STRESS").is_some() {
        return;
    }
    assert!(stats.calls_out < N / 20, "the hits ran out of line: {stats:?}");
}

#[test]
fn a_monomorphic_named_load_runs_inline() {
    let stats = same(
        "function f(o, n) { var s = 0; for (var i = 0; i < n; i++) { s = s + o.x; } return s; }
         f({ x: 3, y: 4 }, 10000)",
        "30000",
    );
    mostly_inline(stats);
}

#[test]
fn a_monomorphic_named_store_runs_inline() {
    let stats = same(
        "function f(o, n) { for (var i = 0; i < n; i++) { o.y = i; } return o.y + o.x; }
         f({ x: 1, y: 0 }, 10000)",
        "10000",
    );
    mostly_inline(stats);
}

#[test]
fn polymorphic_loads_and_stores_run_inline() {
    let stats = same(
        "function f(os, n) {
           var s = 0;
           for (var i = 0; i < n; i++) { var o = os[i & 3]; o.v = i; s = s + o.v + o.k; }
           return s;
         }
         f([{ k: 1, v: 0 }, { v: 0, k: 2 }, { a: 0, k: 3, v: 0 }, { b: 0, c: 0, v: 0, k: 4 }], 10000)",
        &((0..10_000u64).map(|i| i + (i & 3) + 1).sum::<u64>().to_string()),
    );
    mostly_inline(stats);
}

#[test]
fn prototype_loads_run_inline_and_follow_prototype_changes() {
    let stats = same(
        "function P() {} P.prototype.m = 2;
         function Q() {} Q.prototype.m = 5;
         function f(o, n) {
           var s = 0;
           for (var i = 0; i < n; i++) {
             if (i === 3000) P.prototype.m = 3;
             if (i === 6000) Object.setPrototypeOf(o, Q.prototype);
             if (i === 8000) o.m = 7;
             s = s + o.m;
           }
           return s;
         }
         f(new P(), 10000)",
        &(3000 * 2 + 3000 * 3 + 2000 * 5 + 2000 * 7).to_string(),
    );
    mostly_inline(stats);
}

#[test]
fn array_lengths_and_dense_elements_run_inline() {
    let stats = same(
        "function f(a) {
           var s = 0;
           for (var k = 0; k < 100; k++)
             for (var i = 0; i < a.length; i++) { a[i] = a[i] + 1; s = s + a[i]; }
           return s;
         }
         f(Array.from({ length: 100 }, (_, i) => i))",
        &((1..=100u64).map(|k| (0..100u64).map(|i| i + k).sum::<u64>()).sum::<u64>().to_string()),
    );
    mostly_inline(stats);
}

#[test]
fn slots_past_the_inline_ones_are_read_and_written() {
    let stats = same(
        "function f(o, n) { var s = 0; for (var i = 0; i < n; i++) { o.p7 = o.p7 + 1; s = s + o.p5; } return s + o.p7; }
         f({ p0: 0, p1: 1, p2: 2, p3: 3, p4: 4, p5: 5, p6: 6, p7: 7 }, 10000)",
        "60007",
    );
    mostly_inline(stats);
}

#[test]
fn accessors_and_reconfigured_properties_take_the_generic_path() {
    same(
        "function f(o, n) {
           var s = 0;
           for (var i = 0; i < n; i++) {
             if (i === 100) Object.defineProperty(o, 'x', { get() { return 10; }, configurable: true });
             if (i === 200) Object.defineProperty(o, 'x', { value: 20, writable: false });
             if (i === 300) delete o.x;
             s = s + (o.x === undefined ? 1000 : o.x);
             o.x = 4;
           }
           return s + ':' + o.x;
         }
         f({ x: 1 }, 400)",
        &format!("{}:4", 1 + 99 * 4 + 100 * 10 + 100 * 20 + 1000 + 99 * 4),
    );
}

#[test]
fn frozen_and_non_writable_properties_are_not_written() {
    same(
        "'use strict';
         function w(o, n) {
           var r = 0;
           for (var i = 0; i < n; i++) { try { o.x = i; } catch (e) { r++; } }
           return r + ':' + o.x;
         }
         var a = { x: 1 }, b = Object.freeze({ x: 1 }), c = Object.defineProperty({}, 'x', { value: 5 });
         var arr = Object.freeze([1, 2, 3]);
         function we(a, n) { var r = 0; for (var i = 0; i < n; i++) { try { a[1] = i; } catch (e) { r++; } } return r + ':' + a[1]; }
         [w(a, 50), w(b, 50), w(c, 50), we(arr, 50), we([1, 2, 3], 50)].join(',')",
        "0:49,50:1,50:5,50:2,0:49",
    );
}

#[test]
fn dictionary_mode_objects_take_the_generic_path() {
    same(
        "var o = {}; for (var i = 0; i < 200; i++) o['k' + i] = i;
         for (var i = 0; i < 150; i++) delete o['k' + i];
         o.x = 1;
         function f(o, n) { var s = 0; for (var i = 0; i < n; i++) { o.x = o.x + 1; s = s + o.k199; } return s + ':' + o.x; }
         f(o, 1000)",
        "199000:1001",
    );
}

#[test]
fn exotic_receivers_take_the_generic_path() {
    same(
        "function len(o) { var s = 0; for (var i = 0; i < 20; i++) s = s + o.length; return s; }
         function nx(o) { var s = 0; for (var i = 0; i < 20; i++) { o.x = i; s = s + o.x; } return s; }
         var ta = new Uint8Array(4); ta.x = 0;
         var px = new Proxy({ x: 0, length: 9 }, { get(t, k) { return k === 'x' ? 3 : t[k]; } });
         var so = new String('abcd'); so.x = 0;
         function sloppy() { return [len(arguments), nx(arguments)]; }
         function strict() { 'use strict'; return [len(arguments), nx(arguments)]; }
         var lenObj = { length: 2 };
         [len([1, 2, 3]), len('abcde'), len(so), len(ta), len(px), len(lenObj),
          nx(ta), nx(px), nx(so), sloppy(1, 2).join('/'), strict(1, 2, 3).join('/')].join(',')",
        "60,100,80,80,180,40,190,60,190,40/190,60/190",
    );
}

#[test]
fn keyed_edge_cases_take_the_generic_path() {
    same(
        "function g(a, k) { var r = []; for (var i = 0; i < 3; i++) r.push(String(a[k])); return r[2]; }
         function s(a, k, v) { for (var i = 0; i < 3; i++) a[k] = v; return String(a[k]); }
         var a = [10, , 30];
         var sp = []; sp[100000] = 7; sp[1] = 8;
         var ta = new Int16Array([1, 2, 3]);
         function sl() { return [g(arguments, 0), s(arguments, 1, 9), arguments[1]].join('/'); }
         function st() { 'use strict'; return [g(arguments, 0), s(arguments, 1, 9), arguments[1]].join('/'); }
         var o = { 0: 'z', 1.5: 'h', '-1': 'm' };
         Object.prototype[1] = 'proto';
         var r = [g(a, 0), g(a, 1), g(a, -0), g(a, NaN), g(a, 1.5), g(a, -1), g(a, 3), g(a, 4294967295),
                  g(sp, 1), g(sp, 100000), g(ta, 2), g('xyz', 1), g(o, 0), g(o, 1.5), g(o, -1),
                  s(a, 1, 'hole'), s(a, 5, 'grow'), a.length, s(sp, 1, 'q'), s(ta, 0, 300.7), s(ta, 9, 1),
                  sl(1, 2), st(1, 2)];
         delete Object.prototype[1];
         r.join(',')",
        "10,proto,10,undefined,undefined,undefined,undefined,undefined,8,7,3,y,z,h,m,hole,grow,6,q,300,undefined,1/9/9,1/9/9",
    );
}

#[test]
fn a_tiered_loop_that_mostly_calls_out_goes_back_to_the_interpreter() {
    // The additions look numeric to the static estimate but concatenate
    // strings, so every one calls out.
    let src = "function f(n) { var t = 0; for (var r = 0; r < n; r++) { var s = '';
                 for (var i = 0; i < 1000; i++) { s += `item-${i};`; } t += s.length; } return t; }
               f(40)";
    let (on, stats) = run(JitMode::On, src);
    assert_eq!(on, run(JitMode::Off, src).0);
    assert!(stats.compiled >= 1 && stats.abandoned >= 1, "{stats:?}");
    // A loop that runs inline stays compiled.
    let (_, stats) = run(
        JitMode::On,
        "function g(o, n) { var s = 0; for (var i = 0; i < n; i++) { s = s + o.x; } return s; } g({ x: 1 }, 100000)",
    );
    assert!(stats.compiled >= 1 && stats.abandoned == 0, "{stats:?}");
}

#[test]
fn a_tiered_loop_that_calls_a_function_every_iteration_calls_it_in_machine_code() {
    // Mostly inline work and a call of a small function. Each call used to
    // leave compiled code, which cost more than the inline work saved, so
    // the loop went back to the interpreter. Since TASK-101.3.3 the callee
    // is compiled once the loop has called it often enough, and the loop
    // then calls it without leaving compiled code.
    let src = "function h(x) { return x & 0xffff; }
               function f(o, n) { var s = 0;
                 for (var i = 0; i < n; i++) { s = s + o.x + o.y + o.x * o.y + (i & 7) * 3 + (i | 1); s = h(s); }
                 return s; }
               f({ x: 3, y: 4 }, 200000)";
    let (on, stats) = run(JitMode::On, src);
    assert_eq!(on, run(JitMode::Off, src).0);
    // Under AGNIJS_GC_STRESS every back edge and every return calls out for
    // the safe point, and the loop is rightly given up; the value is still
    // checked above, the bounds only hold without stress.
    if std::env::var_os("AGNIJS_GC_STRESS").is_some() {
        return;
    }
    assert!(stats.compiled >= 2 && stats.abandoned == 0, "{stats:?}");
    assert!(stats.direct_calls > 150_000, "{stats:?}");
}

#[test]
fn heap_values_survive_collections_at_every_call_out_from_compiled_code() {
    // Each iteration stores and loads inline around a call that allocates
    // (and so, under stress, collects and grows the object arena): compiled
    // code must read the arena afresh after the call.
    let src = "function mk(i) { var t = []; for (var j = 0; j < 4; j++) t.push({ j: j, s: 'w' + i }); return t; }
               function work(n) {
                 var o = { a: 0, b: null }, keep = [], s = 0;
                 for (var i = 0; i < n; i++) {
                   o.a = i;
                   o.b = mk(i);
                   if (i % 5 === 0) keep.push(o.b);
                   s = s + o.a + o.b[3].j + o.b[0].s.length;
                   o.b[1] = { j: i };
                   s = s + o.b[1].j;
                 }
                 for (var k = 0; k < keep.length; k++) s = s + keep[k][1].j + keep[k][2].s.length;
                 return s;
               }
               work(400)";
    let mut out = Vec::new();
    let mut stats = JitStats::default();
    for mode in [JitMode::Off, JitMode::Force] {
        let mut vm = engine(mode);
        vm.heap.stress = true;
        out.push(run_in(&mut vm, src));
        stats = vm.jit_stats();
    }
    assert_eq!(out[0], out[1]);
    let expected: u64 = (0..400u64)
        .map(|i| i + 3 + format!("w{i}").len() as u64 + i)
        .sum::<u64>()
        + (0..400u64)
            .filter(|i| i % 5 == 0)
            .map(|i| i + format!("w{i}").len() as u64)
            .sum::<u64>();
    assert_eq!(out[0], expected.to_string());
    assert!(stats.entries > 0 && stats.compiled >= 2, "compiled code ran under stress: {stats:?}");
}
