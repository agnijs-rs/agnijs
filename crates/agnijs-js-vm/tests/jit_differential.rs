//! The baseline JIT computes what the interpreter computes (TASK-101.3).
//! Each program runs in a fresh engine three times: interpreted only, with
//! every eligible block compiled on first use (`JitMode::Force`), and with
//! the hotness threshold (`JitMode::On`). The completion value (or the
//! error) must be the same every time; where a known answer is given, the
//! interpreter must produce it too.
//!
//! Run with `cargo test -p agnijs-js-vm --features jit --test jit_differential`.
//! The rest of the suite also runs under the JIT with `AGNIJS_JIT=force`.
#![cfg(feature = "jit")]

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Error};
use agnijs_js_vm::interp::jit::{JitMode, JitStats};
use agnijs_js_vm::Vm;

fn engine(mode: JitMode) -> Vm {
    let mut vm = Vm::new(TestHost::new());
    Vm::create_realm(&mut vm);
    vm.set_jit_mode(mode);
    vm
}

/// The completion value of `src` as a string, or the error.
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

/// `src` gives the same answer interpreted, forced and tiered; returns
/// the answer and the forced run's statistics.
fn agree(src: &str) -> (String, JitStats) {
    let (off, _) = run(JitMode::Off, src);
    let (forced, stats) = run(JitMode::Force, src);
    assert_eq!(forced, off, "forced JIT vs interpreter: {src}");
    let (tiered, _) = run(JitMode::On, src);
    assert_eq!(tiered, off, "tiered JIT vs interpreter: {src}");
    (off, stats)
}

/// [`agree`], with the answer known.
fn same(src: &str, expected: &str) -> JitStats {
    let (out, stats) = agree(src);
    assert_eq!(out, expected, "{src}");
    stats
}

#[test]
fn a_numeric_loop_runs_compiled() {
    let stats = same(
        "function loopArith(n) {
           var s = 0;
           for (var i = 0; i < n; i++) { s = (s + i * 7) % 1000003; }
           return s;
         }
         loopArith(5000)",
        &{
            let mut s: u64 = 0;
            for i in 0..5000u64 {
                s = (s + i * 7) % 1000003;
            }
            s.to_string()
        },
    );
    assert!(stats.compiled >= 2, "{stats:?}");
    assert!(stats.entries >= 2, "{stats:?}");
    assert!(stats.code_bytes > 0, "{stats:?}");
}

#[test]
fn a_hot_function_is_compiled_only_once_it_is_hot() {
    // Tiering compiles only blocks with a loop: entering compiled code once
    // per call costs more than a straight-line block's dispatch saves.
    let add = "function add(a, b) { for (var k = 0; k < 2; k++) a = a + b; return a; }";
    let src = format!(
        "{add}
               var s = 0;
               for (var i = 0; i < 3000; i++) s = add(s, i);
               s"
    );
    let mut vm = engine(JitMode::On);
    assert_eq!(run_in(&mut vm, &src), "8997000");
    let on = vm.jit_stats();
    assert!(on.compiled >= 1 && on.entries > 0, "{on:?}");
    let mut vm = engine(JitMode::On);
    assert_eq!(run_in(&mut vm, &format!("{add} add(1, 2)")), "5");
    assert_eq!(vm.jit_stats().compiled, 0, "a cold function stays interpreted");
}

#[test]
fn a_hot_loop_that_mostly_calls_out_stays_interpreted_when_tiered() {
    // Every iteration reads a global, a property and an element: compiled
    // code would hand most instructions back to the runtime.
    let src = "var o = { xs: [1, 2, 3, 4] }, s = 0;
               for (var i = 0; i < 50000; i++) { s = s + o.xs[i & 3]; }
               s";
    let mut vm = engine(JitMode::On);
    assert_eq!(run_in(&mut vm, src), "125000");
    let on = vm.jit_stats();
    assert!(on.unprofitable >= 1 && on.compiled == 0, "{on:?}");
    let (forced, stats) = run(JitMode::Force, src);
    assert_eq!(forced, "125000");
    assert!(stats.compiled >= 1, "forcing compiles it anyway: {stats:?}");
}

#[test]
fn number_operators_on_edge_values_match() {
    let (out, _) = agree(
        "var xs = [0, -0, 1, -1, 0.5, NaN, Infinity, -Infinity, 2147483647, 2147483648,
                   -2147483649, 4294967296, 9007199254740993, 1e21, -1e-7, 3.7, -3.7, 31, 32, 33];
         function f(a, b) {
           return [a + b, a - b, a * b, a / b, a % b, a | b, a & b, a ^ b, a << b, a >> b,
                   a >>> b, a < b, a > b, a <= b, a >= b, a === b, a !== b, a == b, a != b,
                   Object.is(a * b, -0), Object.is(a - b, -0), -a, ~a, a ** 2];
         }
         function g(a) {
           var r = [];
           r.push(a + 1, a - 1, a * 3, a / 3, a % 3, a | 0, a & 255, a ^ -1, a << 3, a >> 3,
                  a >>> 3, a < 1, a > 1, a <= 1, a >= 1, a === 1, a !== 1);
           var b = a; b++; r.push(b); b--; b--; r.push(b);
           if (a < 2) r.push('lt'); if (a > 2) r.push('gt'); if (a !== a) r.push('nan');
           if (a) r.push('truthy'); if (!a) r.push('falsy');
           return r.join(',');
         }
         var out = [];
         for (var i = 0; i < xs.length; i++) {
           out.push(g(xs[i]));
           for (var j = 0; j < xs.length; j++) out.push(f(xs[i], xs[j]).join(','));
         }
         out.join(';')",
    );
    assert!(out.contains("NaN") && out.contains("Infinity"));
}

#[test]
fn mixed_type_operators_take_the_generic_path() {
    same(
        "function f(a, b) { return [a + b, a - b, a < b, a == b, a === b, a | b, -a].join('|'); }
         var vs = ['10', 9, null, undefined, true, '', 'x', 2n, [1], {}];
         var out = [];
         for (var i = 0; i < vs.length; i++)
           for (var j = 0; j < vs.length; j++) {
             try { out.push(f(vs[i], vs[j])); } catch (e) { out.push(e.constructor.name); }
           }
         out.length + ':' + out[0] + ':' + out[1] + ':' + out[77]",
        "100:1010|0|false|true|true|10|-10:109|1|false|false|false|11|-10:4|0|false|true|true|2|-2",
    );
}

#[test]
fn truthiness_and_nullish_branches_match() {
    same(
        "function t(v) {
           var r = '';
           if (v) r += 'T'; else r += 'F';
           if (v == null) r += 'n';
           if (v === undefined) r += 'u';
           r += (v ?? 'd') === v ? '=' : 'd';
           return r;
         }
         [0, -0, NaN, '', 'a', null, undefined, {}, [], 1n, 0n, false, true, Symbol()]
           .map(t).join(' ')",
        "F= F= Fd F= T= Fnd Fnud T= T= T= F= F= T= T=",
    );
}

#[test]
fn property_access_through_inline_caches_matches() {
    same(
        "function Point(x, y) { this.x = x; this.y = y; }
         Point.prototype.len2 = function () { return this.x * this.x + this.y * this.y; };
         var o = { get g() { return this.x + 1; }, set s(v) { this.x = v * 2; }, x: 1 };
         var s = 0;
         for (var i = 0; i < 2000; i++) {
           var p = new Point(i, i + 1);
           s += p.len2();
           o.s = i; s += o.g;
           if (i % 500 === 0) p.z = 1;
           s += p.z === undefined ? 0 : 1;
         }
         var a = [];
         for (var i = 0; i < 100; i++) a[i] = i * 2;
         var t = 0;
         for (var i = 0; i < 110; i++) t += a[i] === undefined ? -1 : a[i];
         s + ':' + t + ':' + a.length",
        "5337334004:9890:100",
    );
}

#[test]
fn calls_closures_and_recursion_match() {
    same(
        "function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }
         function counter() { var c = 0; return function () { return ++c; }; }
         var k = counter(); for (var i = 0; i < 1000; i++) k();
         function sum() { var s = 0; for (var i = 0; i < arguments.length; i++) s += arguments[i]; return s; }
         function rest(a, ...r) { return a + r.length; }
         function dflt(a, b = a * 2) { return a + b; }
         var arrow = (x) => x * 3;
         var obj = { m(x) { return this.base + x; }, base: 100 };
         [fib(20), k(), sum(1, 2, 3, 4), rest(1, 2, 3), dflt(5), arrow(7), obj.m(1),
          [1, 2, 3].map(arrow).reduce((a, b) => a + b, 0), Math.max(3, 9, 4)].join(',')",
        "6765,1001,10,3,15,21,101,18,9",
    );
}

#[test]
fn calls_between_compiled_functions_stay_in_compiled_code() {
    // fib(15) makes 1973 calls, 1972 of them from fib itself; forced, each
    // enters the callee's compiled code from the caller's without going
    // back to the tier loop, and returns straight into the caller's.
    let stats = same("function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); } fib(15)", "610");
    assert!(stats.direct_calls >= 1972, "{stats:?}");
    assert!(stats.entries < 10, "no call or return goes back to the tier loop: {stats:?}");
}

#[test]
fn a_throw_in_a_nested_callee_is_caught_several_compiled_frames_up() {
    // catcher -> a -> b -> c all compiled and entered directly; c throws
    // every fifth call, and the handler in catcher, three compiled callers
    // up, takes it; the loop then goes on calling directly.
    let stats = same(
        "function c(i) { if (i % 5 === 4) throw new RangeError('r' + i); return i; }
         function b(i) { var t = c(i); return t + 1; }
         function a(i) { var t = b(i); return t * 2; }
         function catcher(n) {
           var s = 0, caught = 0, last = '';
           for (var i = 0; i < n; i++) {
             try { s += a(i); } catch (e) { caught++; last = e.message; }
           }
           return s + '/' + caught + '/' + last;
         }
         catcher(500)",
        "200000/100/r499",
    );
    assert!(stats.direct_calls >= 1000, "{stats:?}");
}

#[test]
fn this_in_compiled_code_is_each_frames_own() {
    // Methods, sloppy and strict functions called with primitive and
    // nullish receivers, and arrows reading an outer `this`, all compiled
    // and entered directly from each other.
    let stats = same(
        "function sloppy() { return typeof this + (this === globalThis ? 'G' : ''); }
         function strict() { 'use strict'; return String(this); }
         var o = { n: 3, m() { return this.n + (() => this.n)(); } };
         function run(k) {
           var s = '';
           for (var i = 0; i < k; i++) {
             s = sloppy() + sloppy.call(1) + strict.call(2) + strict() + o.m();
           }
           return s;
         }
         run(300)",
        "objectGobject2undefined6",
    );
    assert!(stats.direct_calls >= 300, "{stats:?}");
}

#[test]
fn recursion_deeper_than_the_nesting_bound_runs_and_overflows_alike() {
    // Past the bound, calls go back through the tier loop and nest again
    // from there; the frame limit still ends unbounded recursion with a
    // RangeError the compiled caller can catch.
    let stats = same(
        "function sum(n) { return n === 0 ? 0 : n + sum(n - 1); }
         function down(n) { return down(n + 1) + 1; }
         var r; try { down(0); } catch (e) { r = e.constructor.name; }
         sum(5000) + ',' + r + ',' + sum(10)",
        "12502500,RangeError,55",
    );
    assert!(stats.direct_calls >= 5000, "{stats:?}");
}

#[test]
fn collections_at_every_safe_point_keep_nested_compiled_frames_values() {
    let src = "function leaf(o, i) { return { v: o.v + i, s: o.s + i }; }
               function mid(o, i) { var p = leaf(o, i); var q = leaf(p, 1); return [p, q]; }
               function top(n) {
                 var o = { v: 0, s: '' }, keep = [];
                 for (var i = 0; i < n; i++) {
                   var pq = mid(o, i);
                   o = { v: pq[1].v % 1000, s: pq[0].s.slice(-4) };
                   if (i % 10 === 0) keep.push(pq[0]);
                 }
                 var t = 0; for (var j = 0; j < keep.length; j++) t += keep[j].v;
                 return o.v + ',' + o.s + ',' + t;
               }
               function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }
               top(200) + ',' + fib(12)";
    let mut out = Vec::new();
    for mode in [JitMode::Off, JitMode::Force] {
        let mut vm = engine(mode);
        vm.heap.stress = true;
        out.push(run_in(&mut vm, src));
        if mode == JitMode::Force {
            assert!(vm.jit_stats().direct_calls > 400, "{:?}", vm.jit_stats());
        }
    }
    assert_eq!(out[0], out[1]);
}

#[test]
fn an_interrupt_stops_a_loop_in_a_nested_callee() {
    let host = TestHost::new();
    let mut vm = Vm::new(host.clone());
    Vm::create_realm(&mut vm);
    vm.set_jit_mode(JitMode::Force);
    let realm = vm.main_realm();
    run_in(&mut vm, "function id(x) { return x; } function spin() { for (var i = 0; ; i++) id(i); } id(1)");
    host.interrupt_after.set(Some(host.interrupt_polls.get() + 50));
    let r = vm.eval_script(realm, "spin.js", "function outer() { return spin(); } outer()");
    assert!(matches!(r, Err(Error::Interrupted)), "{r:?}");
    assert!(vm.jit_stats().direct_calls > 3, "{:?}", vm.jit_stats());
    host.interrupt_after.set(None);
    assert_eq!(run_in(&mut vm, "function f(n) { return n ? f(n - 1) + 1 : 0; } f(30)"), "30");
}

#[test]
fn exceptions_unwind_through_compiled_frames() {
    same(
        "function thrower(i) { if (i % 7 === 3) throw new Error('e' + i); return i; }
         function catcher(n) {
           var s = 0, caught = 0;
           for (var i = 0; i < n; i++) {
             try { s += thrower(i); } catch (e) { caught++; } finally { s += 1; }
           }
           return s + '/' + caught;
         }
         function tdz() { try { x; let x = 1; } catch (e) { return e.constructor.name; } }
         function deep(n) { return deep(n + 1) + 1; }
         var r;
         try { deep(0); } catch (e) { r = e.constructor.name; }
         function uncaught() { for (var i = 0; ; i++) if (i === 50) throw new TypeError('t'); }
         var u; try { uncaught(); } catch (e) { u = e.message; }
         [catcher(1000), tdz(), r, u].join(',')",
        "429000/143,ReferenceError,RangeError,t",
    );
}

#[test]
fn an_uncaught_throw_from_compiled_code_reaches_the_host() {
    let (out, _) = agree("function f() { for (var i = 0; i < 10; i++) {} throw 42; } f()");
    assert!(out.starts_with("error:"), "{out}");
}

#[test]
fn ineligible_functions_run_interpreted_next_to_compiled_ones() {
    same(
        "function* gen(n) { for (var i = 0; i < n; i++) yield i; }
         async function af(x) { var a = await x; return a + 1; }
         class A { constructor(v) { this.v = v; } get dbl() { return this.v * 2; } }
         class B extends A { constructor(v) { super(v + 1); } }
         function withEval(s) { return eval(s); }
         var s = 0;
         for (var v of gen(100)) s += v;
         var r = 0;
         af(41).then(v => { r = v; });
         var bs = 0;
         for (var i = 0; i < 100; i++) bs += new B(i).dbl;
         globalThis.done = () => [s, bs, withEval('1 + 2'), typeof r].join(',');
         done()",
        "4950,10100,3,number",
    );
}

#[test]
fn globals_bindings_and_control_flow_match() {
    same(
        "var g = 0; let l = 0; const c = 3;
         function bump() { g += c; l++; }
         for (var i = 0; i < 1000; i++) bump();
         function sw(x) {
           switch (x % 5) { case 0: return 'a'; case 1: case 2: return 'b'; default: return 'c'; }
         }
         var w = ''; for (var i = 0; i < 10; i++) w += sw(i);
         var lab = 0;
         outer: for (var i = 0; i < 20; i++) {
           for (var j = 0; j < 20; j++) { if (j > i) continue outer; if (i * j > 100) break outer; lab++; }
         }
         var keys = []; for (var k in { p: 1, q: 2 }) keys.push(k);
         var m = 0; do { m++; } while (m < 77);
         var t = typeof undefinedThing + (1 in [5, 6]) + ('p' in { p: 1 }) + ([] instanceof Array);
         [g, l, w, lab, keys.join(''), m, t].join(',')",
        "3000,1000,abbccabbcc,76,pq,77,undefinedtruetruetrue",
    );
}

#[test]
fn an_interrupt_stops_a_compiled_loop() {
    let host = TestHost::new();
    let mut vm = Vm::new(host.clone());
    Vm::create_realm(&mut vm);
    vm.set_jit_mode(JitMode::Force);
    let realm = vm.main_realm();
    host.interrupt_after.set(Some(host.interrupt_polls.get() + 3));
    let r = vm.eval_script(realm, "spin.js", "function spin() { for (;;) {} } spin()");
    assert!(matches!(r, Err(Error::Interrupted)), "{r:?}");
    assert!(vm.jit_stats().entries > 0, "the loop ran compiled");
    host.interrupt_after.set(None);
    assert_eq!(run_in(&mut vm, "6 * 7"), "42");
}

#[test]
fn collections_at_every_safe_point_keep_compiled_frames_values() {
    let src = "function mk(i) { return { i: i, s: 'v' + i, a: [i, i + 1] }; }
               function work(n) {
                 var keep = [], s = 0;
                 for (var i = 0; i < n; i++) {
                   var o = mk(i);
                   if (i % 3 === 0) keep.push(o);
                   s += o.a[1] - o.i + o.s.length;
                 }
                 for (var j = 0; j < keep.length; j++) s += keep[j].a[0];
                 return s;
               }
               work(300)";
    let mut out = Vec::new();
    for mode in [JitMode::Off, JitMode::Force] {
        let mut vm = engine(mode);
        vm.heap.stress = true;
        out.push(run_in(&mut vm, src));
    }
    assert_eq!(out[0], out[1]);
    assert_eq!(out[0], "16240");
}
