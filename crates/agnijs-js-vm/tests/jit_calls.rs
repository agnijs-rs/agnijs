//! Calls, environment slots and global bindings done by machine code
//! itself (TASK-101.3.3): a compiled function pushes its callee's frame,
//! runs the callee's machine code on the native stack and pops the frame,
//! and reads environment slots and global bindings where they are.
//!
//! Each program runs interpreted, tiered, and forced with every part on,
//! every part off and each part alone, in fresh engines; all answers must
//! be the same, and equal the expected one where it is given.
//!
//! Run with `cargo test -p agnijs-js-vm --features jit --test jit_calls`.
#![cfg(feature = "jit")]

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Error};
use agnijs_js_vm::interp::jit::{JitMode, JitStats, DIRECT_CALLS, DIRECT_ENVS, DIRECT_GLOBALS};
use agnijs_js_vm::Vm;

const ALL: u32 = DIRECT_CALLS | DIRECT_ENVS | DIRECT_GLOBALS;

fn engine(mode: JitMode, direct: u32) -> Vm {
    let mut vm = Vm::new(TestHost::new());
    Vm::create_realm(&mut vm);
    vm.set_jit_mode(mode);
    vm.set_jit_direct(direct);
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

/// Whether the whole process collects at every safe point
/// (`AGNIJS_GC_STRESS`): bounds on the statistics do not hold then.
fn stressed() -> bool {
    std::env::var_os("AGNIJS_GC_STRESS").is_some_and(|v| !v.is_empty() && v != "0")
}

fn run(mode: JitMode, direct: u32, src: &str) -> (String, JitStats) {
    let mut vm = engine(mode, direct);
    let out = run_in(&mut vm, src);
    (out, vm.jit_stats())
}

/// `src` gives `expected` every way; returns the statistics of the forced
/// run with everything on.
fn same(src: &str, expected: &str) -> JitStats {
    let (off, _) = run(JitMode::Off, ALL, src);
    assert_eq!(off, expected, "interpreter: {src}");
    let (tiered, _) = run(JitMode::On, ALL, src);
    assert_eq!(tiered, off, "tiered: {src}");
    for direct in [
        0,
        DIRECT_CALLS,
        DIRECT_ENVS,
        DIRECT_GLOBALS,
        DIRECT_CALLS | DIRECT_ENVS,
    ] {
        let (forced, _) = run(JitMode::Force, direct, src);
        assert_eq!(forced, off, "forced, direct {direct:#b}: {src}");
    }
    let (forced, stats) = run(JitMode::Force, ALL, src);
    assert_eq!(forced, off, "forced: {src}");
    stats
}

#[test]
fn machine_code_calls_compiled_functions_itself() {
    // fib(18) makes 8361 calls; all but the first few are made by machine
    // code, with no call out for the call or the return.
    let stats = same(
        "function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); } fib(18)",
        "2584",
    );
    assert!(stats.direct_calls >= 8300, "{stats:?}");
    // Under AGNIJS_GC_STRESS a safe point is wanted whenever a call
    // returns, so compiled code calls out for it by design; the value is
    // still checked above, the bound only holds without stress.
    if stressed() {
        return;
    }
    assert!(
        stats.calls_out < 2000,
        "calls and returns stay in machine code: {stats:?}"
    );
    assert!(stats.entries < 20, "{stats:?}");
}

#[test]
fn a_site_that_calls_many_functions_calls_each_directly() {
    // One call site, eight callees in turn: the callee table serves them
    // all, where a cache per site would have given up.
    let stats = same(
        "var fs = [];
         for (let k = 0; k < 8; k++) fs.push(function (x) { return x * (k + 1); });
         function make(k) { return function (x) { return x + k; }; }
         var gs = [make(1), make(2), make(3)];
         function apply(f, x) { return f(x); }
         function run(n) {
           var s = 0;
           for (var i = 0; i < n; i++) s = (s + apply(fs[i % 8], i) + apply(gs[i % 3], i)) % 1000003;
           return s;
         }
         run(4000)",
        "17867",
    );
    assert!(stats.direct_calls >= 15000, "{stats:?}");
}

#[test]
fn arguments_are_copied_dropped_and_missing_as_the_callee_declares() {
    same(
        "function two(a, b) { return String(a) + '|' + String(b); }
         function none() { return arguments.length; }
         function rest(a, ...r) { return a + ':' + r.join('+'); }
         function dflt(a, b = a + 1, c = b * 2) { return a + b + c; }
         function locals(a) { var x, y = a; return typeof x + typeof y + (x === undefined); }
         function run(n) {
           var out = '';
           for (var i = 0; i < n; i++) {
             out = [two(), two(i), two(i, i + 1), two(i, i + 1, i + 2, i + 3),
                    none(), none(1, 2, 3), rest(i), rest(i, 1, 2), dflt(i), dflt(i, 1), dflt(i, 1, 2),
                    locals(i), locals()].join(',');
           }
           return out;
         }
         run(50)",
        "undefined|undefined,49|undefined,49|50,49|50,0,3,49:,49:1+2,199,52,52,undefinednumbertrue,undefinedundefinedtrue",
    );
}

#[test]
fn this_is_passed_boxed_or_replaced_as_the_callee_takes_it() {
    same(
        "function sloppy() { return this === globalThis ? 'G' : typeof this + ':' + String(this); }
         function strict() { 'use strict'; return this === undefined ? 'U' : typeof this + ':' + String(this); }
         var o = { toString() { return 'o'; }, sloppy, strict, arrow: () => typeof this };
         var s = sloppy, t = strict;
         function run(n) {
           var out = '';
           for (var i = 0; i < n; i++) {
             out = [s(), t(), o.sloppy(), o.strict(), o.arrow(),
                    sloppy.call(null), sloppy.call(7), strict.call(7), sloppy.call('x'), strict.call(o),
                    (0, o.sloppy)(), (0, o.strict)()].join(',');
           }
           return out;
         }
         run(60)",
        "G,U,object:o,object:o,object,G,object:7,number:7,object:x,object:o,G,U",
    );
}

#[test]
fn the_register_file_may_move_under_a_function_called_directly() {
    // `grow` recurses deeper each round, so the register file is
    // reallocated while `step`'s caller waits in a direct call; `step`
    // then goes on reading and writing its registers, through call outs
    // that leave what it caches as it is (a global, an environment slot,
    // its own function) and ones that do not (the keyed loads).
    let stats = same(
        "var bias = 3, sym = Symbol('k');
         function grow(n) { return n > 0 ? grow(n - 1) + 1 : 0; }
         var step = function me(o, n, k) {
           var a = grow(n);
           var b = bias;
           var f = me;
           var c = o[sym];
           var d = grow(n >> 1) + k();
           var e = o[sym] + (f === me ? 1 : 0);
           return a + b + c + d + e;
         };
         function run(rounds) {
           var o = { [sym]: 5 }, s = 0, kk = 0, k = () => ++kk;
           for (var r = 1; r <= rounds; r++) s += step(o, r * 3, k);
           return s + '/' + kk;
         }
         run(120)",
        "41580/120",
    );
    assert!(stats.direct_calls > 20000, "{stats:?}");
}

#[test]
fn recursion_past_the_stack_bound_unwinds_and_goes_on() {
    // Machine frames nest on the native stack only so far; past the bound
    // the frames continue from memory, and the frame limit still ends
    // unbounded recursion with a RangeError a compiled caller catches.
    let stats = same(
        "function sum(n) { return n === 0 ? 0 : n + sum(n - 1); }
         function even(n) { return n === 0 ? true : odd(n - 1); }
         function odd(n) { return n === 0 ? false : even(n - 1); }
         function down(n) { return down(n + 1) + 1; }
         var r; try { down(0); } catch (e) { r = e.constructor.name; }
         [sum(9000), even(9001), odd(9001), r, sum(12)].join(',')",
        "40504500,false,true,RangeError,78",
    );
    assert!(stats.direct_calls >= 25000, "{stats:?}");
}

#[test]
fn machine_frames_do_not_nest_past_the_hosts_stack_budget() {
    // The host's stack budget (`Engine::set_stack_budget`) is the floor
    // when it is nearer than the JIT's own bound. With none of it left
    // where compiled code is entered, no call is made on the native stack:
    // each goes through the runtime, and the answer is the same.
    let src = "function sum(n) { return n === 0 ? 0 : n + sum(n - 1); } sum(3000)";
    let run = |budget: Option<usize>| {
        let mut vm = engine(JitMode::Force, ALL);
        let realm = vm.main_realm();
        let code = vm.compile_script(realm, "t.js", src).expect("compiles");
        if let Some(bytes) = budget {
            vm.set_stack_budget(Some(bytes)).expect("a budget");
        }
        let out = match vm.eval_bytecode(realm, &code) {
            Ok(v) => format!("{v:?}"),
            Err(e) => format!("error: {e}"),
        };
        (out, vm.jit_stats())
    };
    let (roomy, stats) = run(None);
    assert!(!roomy.starts_with("error"), "{roomy}");
    assert!(stats.direct_calls >= 2900, "{stats:?}");
    let (tight, stats) = run(Some(0));
    assert_eq!(tight, roomy);
    assert!(stats.compiled >= 1 && stats.direct_calls == 0, "{stats:?}");
}

#[test]
fn a_throw_passes_through_frames_machine_code_pushed() {
    let stats = same(
        "function leaf(i) { if (i % 7 === 3) throw new RangeError('r' + i); return i; }
         function mid(i) { var t = leaf(i); return t + 1; }
         function guarded(i) { try { return mid(i); } finally { fin++; } }
         function top(i) { var t = guarded(i); return t * 2; }
         var fin = 0;
         function run(n) {
           var s = 0, caught = 0, last = '';
           for (var i = 0; i < n; i++) {
             try { s += top(i); } catch (e) { caught++; last = e.message; }
           }
           return [s, caught, last, fin].join('/');
         }
         run(700)",
        "420600/100/r696/700",
    );
    assert!(stats.direct_calls >= 2000, "{stats:?}");
}

#[test]
fn collections_find_the_frames_machine_code_pushed() {
    // Values live only in registers of frames pushed by machine code, and
    // in `this`, across allocating callees: collections run at safe points
    // inside the callees while those frames wait.
    let src = "function Node(v, next) { this.v = v; this.next = next; }
               Node.prototype.sum = function () { var s = 0, n = this; while (n) { s += n.v; n = n.next; } return s; };
               function build(n, seed) { var l = null; for (var i = 0; i < n; i++) l = new Node(seed + i, l); return l; }
               function leaf(a, b) { var x = build(40, a), y = build(40, b); return x.sum() + y.sum(); }
               function mid(a) { var keep = build(30, a); var t = leaf(a, a + 1); return t + keep.sum(); }
               function top(a) { var k1 = { a: a }, k2 = [a, a + 1]; var t = mid(a) + mid(a + 2); return t + k1.a + k2[1]; }
               function run(n) { var s = 0; for (var i = 0; i < n; i++) s = (s + top(i)) % 1000003; return s; }
               run(600)";
    let mut out = Vec::new();
    for (mode, direct) in [
        (JitMode::Off, ALL),
        (JitMode::Force, ALL),
        (JitMode::Force, DIRECT_CALLS),
    ] {
        let mut vm = engine(mode, direct);
        let before = vm.gc_count;
        out.push(run_in(&mut vm, src));
        assert!(
            vm.gc_count - before >= 8,
            "collections ran: {}",
            vm.gc_count - before
        );
        if mode == JitMode::Force {
            assert!(vm.jit_stats().direct_calls > 1000, "{:?}", vm.jit_stats());
        }
    }
    assert_eq!(out[0], "467874");
    assert_eq!(out[0], out[1]);
    assert_eq!(out[0], out[2]);
}

#[test]
fn collections_at_every_safe_point_agree_with_direct_calls_compiled_in() {
    // Under GC stress no call is made directly (a safe point is always
    // wanted), but the code is the code that would: the paths around the
    // direct call run with a collection at every call out.
    let src = "function pair(a, b) { return { a: a, b: b }; }
               function swap(p) { return pair(p.b, p.a); }
               function run(n) { var p = pair('x', 1); for (var i = 0; i < n; i++) p = swap(pair(p.a + i, p.b)); return p.a + '|' + p.b; }
               run(40)";
    let mut out = Vec::new();
    for mode in [JitMode::Off, JitMode::Force] {
        let mut vm = engine(mode, ALL);
        vm.heap.stress = true;
        out.push(run_in(&mut vm, src));
    }
    assert_eq!(out[0], out[1]);
}

#[test]
fn an_interrupt_stops_recursion_made_of_direct_calls() {
    let host = TestHost::new();
    let mut vm = Vm::new(host.clone());
    Vm::create_realm(&mut vm);
    vm.set_jit_mode(JitMode::Force);
    let realm = vm.main_realm();
    // Warm up: compiled and in the callee table.
    assert_eq!(
        run_in(
            &mut vm,
            "function tree(n) { return n === 0 ? 1 : tree(n - 1) + tree(n - 1); } tree(10)"
        ),
        "1024"
    );
    let direct = vm.jit_stats().direct_calls;
    assert!(direct > 1000, "{:?}", vm.jit_stats());
    // No loop anywhere: the polls are the ones at function entry and
    // after a return, which machine code counts.
    host.interrupt_after
        .set(Some(host.interrupt_polls.get() + 20));
    let r = vm.eval_script(realm, "spin.js", "tree(40)");
    assert!(matches!(r, Err(Error::Interrupted)), "{r:?}");
    assert!(
        vm.jit_stats().direct_calls > direct + 30_000,
        "{:?}",
        vm.jit_stats()
    );
    host.interrupt_after.set(None);
    assert_eq!(run_in(&mut vm, "tree(8)"), "256");
}

#[test]
fn environment_slots_are_read_and_written_where_they_are() {
    same(
        "function counter(start) { var c = start; return { inc(d) { c += d; return c; }, get() { return c; } }; }
         function nest(a) { return function (b) { return function (c) { return function (d) { a += d; return a + b + c + d; }; }; }; }
         function tdz() { var f = () => x; try { f(); } catch (e) { var m = e.constructor.name; } let x = 4; return m + f(); }
         function blocks(n) { var fs = []; for (let i = 0; i < n; i++) { let j = i * 2; fs.push(() => i + j); } return fs.map((f) => f()).join(''); }
         function dyn(o) { var v = 1; with (o) { var f = function () { return v + w; }; } return f(); }
         function evaled() { var q = 5; var f = function () { return eval('q + 1'); }; return f(); }
         var w = 100;
         function run(n) {
           var c = counter(10), deep = nest(1)(2)(3), s = 0;
           for (var i = 0; i < n; i++) { c.inc(i); s += deep(i); }
           return [c.get(), s, tdz(), blocks(4), dyn({ v: 7 }), dyn({ w: 1 }), evaled()].join(',');
         }
         run(200)",
        "19910,1354400,ReferenceError4,0369,107,2,6",
    );
}

#[test]
fn global_bindings_are_read_where_they_are_until_they_move() {
    same(
        "var g = 1; let lex = 10; const k = 100;
         function read() { return g + lex + k; }
         function late() { return typeof later === 'undefined' ? -1 : later; }
         function acc() { return viaGetter; }
         var hits = 0;
         function run(n) {
           var s = 0;
           for (var i = 0; i < n; i++) {
             s += read() + late() + acc();
             if (i === 50) g = 2;
             if (i === 100) lex = 20;
             if (i === 150) globalThis.later = 7;
             if (i === 200) delete globalThis.later;
             if (i === 250) { for (var j = 0; j < 300; j++) globalThis['pad' + j] = j; }
             if (i === 300) Object.defineProperty(globalThis, 'viaGetter', { get() { hits++; return 5; }, configurable: true });
             if (i === 350) Object.defineProperty(globalThis, 'viaGetter', { value: 9, writable: true, configurable: true });
             if (i === 400) { for (var j = 0; j < 300; j += 2) delete globalThis['pad' + j]; }
           }
           return s + '/' + hits;
         }
         globalThis.viaGetter = 0;
         run(500)",
        "61430/50",
    );
}

#[test]
fn a_global_lexical_declared_later_shadows_a_property_a_site_read() {
    // The second script declares a lexical over a global property that a
    // site of the first script's function has cached.
    for (mode, direct) in [
        (JitMode::Off, ALL),
        (JitMode::Force, ALL),
        (JitMode::Force, DIRECT_GLOBALS),
        (JitMode::On, ALL),
    ] {
        let mut vm = engine(mode, direct);
        let first = run_in(
            &mut vm,
            "globalThis.shadowed = 1; function read() { return shadowed; }
             var s = 0; for (var i = 0; i < 50; i++) s += read(); s",
        );
        assert_eq!(first, "50", "{mode:?} {direct}");
        let second = run_in(
            &mut vm,
            "let shadowed = 9; var t = 0; for (var i = 0; i < 50; i++) t += read(); t",
        );
        assert_eq!(second, "450", "{mode:?} {direct}");
        let third = run_in(
            &mut vm,
            "globalThis.shadowed = 3; var u = 0; for (var i = 0; i < 50; i++) u += read(); u",
        );
        assert_eq!(third, "450", "{mode:?} {direct}");
    }
}

#[test]
fn a_function_without_globals_calls_one_that_reads_them() {
    // The run is entered in a block that reads no global binding (a
    // callback run by a native), and calls directly a function that does.
    let stats = same(
        "var scale = 3;
         function scaled(x) { return x * scale; }
         var o = { f: scaled, each(a) { var s = 0; for (var i = 0; i < a.length; i++) s += this.f(a[i]); return s; } };
         function run(n) {
           var a = [1, 2, 3, 4], s = 0;
           for (var i = 0; i < n; i++) s += [0].map(function () { return o.each(a); })[0];
           return s;
         }
         run(300)",
        "9000",
    );
    assert!(stats.direct_calls > 1000, "{stats:?}");
}

#[test]
fn a_function_of_another_realm_runs_in_its_realm_and_returns_to_the_callers() {
    for (mode, direct) in [
        (JitMode::Off, ALL),
        (JitMode::Force, ALL),
        (JitMode::Force, DIRECT_CALLS),
    ] {
        let mut vm = engine(mode, direct);
        let main = vm.main_realm();
        let other = Engine::create_realm(&mut vm).expect("a second realm");
        // `mk` makes arrays of its own realm and reads its own global.
        let f = vm
            .eval_script(
                other,
                "o.js",
                "var tag = 'other'; (function mk(x) { return [tag, x, [] instanceof Array]; })",
            )
            .expect("a function");
        let global = Engine::global_object(&mut vm, main).expect("the global object");
        let key = agnijs_engine_api::PropertyKey::from("mk");
        Engine::set(&mut vm, main, &global, &key, &f).expect("set");
        let out = run_in(
            &mut vm,
            "var tag = 'main';
             function here(x) { return [tag, x, [] instanceof Array]; }
             function run(n) {
               var s = '';
               for (var i = 0; i < n; i++) {
                 var a = mk(i), b = here(i);
                 s = [a[0], a[1], a[2], a instanceof Array, b[0], b[2], b instanceof Array].join(',');
               }
               return s;
             }
             run(200)",
        );
        assert_eq!(
            out, "other,199,true,false,main,true,true",
            "{mode:?} {direct}"
        );
    }
}

#[test]
fn tiered_a_recursive_function_is_compiled_and_calls_itself_in_machine_code() {
    // No loop anywhere near it: before TASK-101.3.3 mode `On` left such a
    // function to the interpreter. It is compiled once it is hot and seen
    // calling itself, and then one run of it makes its calls directly.
    let src = "function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }
               var s = 0; for (var r = 0; r < 4; r++) s += fib(20); s";
    let (on, stats) = run(JitMode::On, ALL, src);
    assert_eq!(on, run(JitMode::Off, ALL, src).0);
    assert_eq!(on, "27060");
    assert!(stats.compiled >= 1, "{stats:?}");
    // Under AGNIJS_GC_STRESS every return calls out for the safe point, a
    // cost the tier's judgement rightly gives the block up for.
    if stressed() {
        return;
    }
    assert!(stats.direct_calls > 50_000, "{stats:?}");
}

#[test]
fn tiered_a_small_function_only_interpreted_code_calls_is_not_compiled() {
    // Hot, without a loop, and called only by a callback a native runs:
    // compiled code would be entered from the interpreter at every call,
    // which costs more than interpreting it. It waits for a compiled
    // caller instead, and nothing is compiled.
    let src = "function leaf(x) { return x * 3 + 1; }
               var total = 0, a = []; for (var i = 0; i < 40; i++) a.push(i);
               for (var r = 0; r < 600; r++) a.forEach(function (x) { total += leaf(x); });
               total";
    let (on, stats) = run(JitMode::On, ALL, src);
    assert_eq!(on, run(JitMode::Off, ALL, src).0);
    assert_eq!(on, "1428000");
    assert_eq!(stats.compiled, 0, "{stats:?}");
    assert_eq!(stats.entries, 0, "{stats:?}");
}
