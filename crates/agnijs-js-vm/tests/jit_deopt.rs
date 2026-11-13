//! Speculation, deoptimization and recompilation in the VM (TASK-101.3.1).
//! The VM guesses, from the live frame, which registers a hot loop's
//! arithmetic will only ever see numbers in; compiled code keeps those
//! unboxed. A guess that fails leaves for the interpreter at the guarded
//! instruction (a side exit, counted in `JitStats::deopts`), and the block
//! is recompiled without it. Every program must compute what the
//! interpreter computes.
//!
//! Run with `cargo test -p agnijs-js-vm --features jit --test jit_deopt`,
//! and with `AGNIJS_GC_STRESS=1` for the collections under optimised frames.
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

/// `src` gives the same answer interpreted and forced; returns the answer
/// and the forced run's statistics.
fn agree(src: &str) -> (String, JitStats) {
    let (off, _) = run(JitMode::Off, src);
    let (forced, stats) = run(JitMode::Force, src);
    assert_eq!(forced, off, "forced JIT vs interpreter: {src}");
    let (tiered, _) = run(JitMode::On, src);
    assert_eq!(tiered, off, "tiered JIT vs interpreter: {src}");
    (off, stats)
}

fn same(src: &str, expected: &str) -> JitStats {
    let (out, stats) = agree(src);
    assert_eq!(out, expected, "{src}");
    stats
}

const SUM: &str = "function sum(xs) {
    var s = 0;
    for (var i = 0; i < xs.length; i++) { var x = xs[i]; s = s + x; }
    return s;
}";

#[test]
fn a_loop_on_numbers_runs_speculated_without_leaving() {
    let stats = same(
        "function f(n) { var s = 0; for (var i = 0; i < n; i++) { s = s + i; } return s; }
         f(2000)",
        "1999000",
    );
    assert!(stats.speculated >= 1, "{stats:?}");
    assert_eq!(stats.deopts, 0, "{stats:?}");
}

#[test]
fn a_non_number_in_a_guessed_register_continues_in_the_interpreter() {
    let stats = same(
        &format!("{SUM} var xs = []; for (var i = 0; i < 300; i++) xs.push(i); xs.push('a'); xs.push(5); sum(xs)"),
        "44850a5",
    );
    assert!(stats.speculated >= 1, "{stats:?}");
    assert!(stats.deopts >= 1, "{stats:?}");
}

#[test]
fn a_failed_guess_is_dropped_when_the_block_is_recompiled() {
    // After the first string, the block is recompiled without the guess
    // about `x`, so the later strings do not leave compiled code again.
    let stats = same(
        &format!(
            "{SUM} var out = '';
             for (var k = 0; k < 20; k++) {{
                 var xs = [1, 2, 3]; for (var i = 0; i < 100; i++) xs.push(k == 0 ? i : 's');
                 out = out + String(sum(xs)).length + ',';
             }}
             out"
        ),
        // 1 + 2 + 3 + 0 + ... + 99 = 4956; then "6" followed by 100 's'.
        &format!("4,{}", "101,".repeat(19)),
    );
    // One guess fails, once: `x` (the string). The block is compiled again
    // at the loop's next back-edge, where `s` already holds a string and
    // is not guessed. (The script's own body guesses nothing: the register
    // its loop compares is not assigned yet and is live at its start. It
    // used to be guessed there, and failed the entry guard at once.)
    assert_eq!(stats.deopts, 1, "{stats:?}");
    assert!(
        stats.compiled >= 3,
        "the script, `sum`, and `sum` again: {stats:?}"
    );
}

#[test]
fn an_argument_that_stops_being_a_number_fails_the_entry_guard() {
    let stats = same(
        "function f(n, x) { var s = 0; for (var i = 0; i < n; i++) { s = s + x; } return s; }
         var a = f(500, 1); var b = f(3, 'q'); var c = f(500, 2); a + '|' + b + '|' + c",
        "500|0qqq|1000",
    );
    assert!(stats.deopts >= 1, "{stats:?}");
}

#[test]
fn a_collection_during_a_callback_under_a_speculated_frame_finds_its_values() {
    // The frame running `f` holds `s` and `i` in machine state while
    // `map`'s callback allocates; under `AGNIJS_GC_STRESS=1` every
    // allocation collects, tracing the compiled frame's registers.
    let stats = same(
        "function f(n) {
             var s = 0, last = null;
             for (var i = 0; i < n; i++) {
                 last = [i, i + 1].map(function (v) { return { v: v * 2 }; });
                 s = s + last[1].v;
             }
             return s + ':' + last[0].v;
         }
         f(400)",
        "160400:798",
    );
    assert_eq!(stats.deopts, 0, "{stats:?}");
}

#[test]
fn a_guess_that_fails_repeatedly_is_given_up() {
    // Several registers are guessed wrong, one call at a time: after the
    // block has been recompiled a few times, it stops guessing at all and
    // is not compiled again for the remaining calls.
    let stats = agree(
        "function g(a, b, c, d, e) {
             var s = 0;
             for (var i = 0; i < 50; i++) { s = s + a + b + c + d + e; }
             return s;
         }
         var r = g(1, 1, 1, 1, 1);
         var args = [1, 1, 1, 1, 1];
         for (var k = 0; k < 5; k++) { args[k] = 'x'; r = r + g.apply(null, args).length; }
         for (var k = 0; k < 5; k++) { args[k] = 1; r = r + g.apply(null, args).length; }
         r",
    )
    .1;
    // One failure per newly wrong argument, until the fourth gives up
    // every guess.
    assert_eq!(stats.deopts, 4, "{stats:?}");
    assert!(
        stats.compiled >= 5,
        "compiled again after each failure: {stats:?}"
    );
}

#[test]
fn an_unassigned_register_that_is_live_at_the_entry_is_not_guessed() {
    // TASK-103.34: the handler reads `x`, and the property load may throw
    // before `x` is assigned, so `x` is live at the function's start and
    // "nothing yet" is no reason to guess it a number there: the guess
    // would fail the entry guard at once. The other loop registers are
    // assigned before anything can throw and are still guessed.
    let (_, stats) = run(JitMode::Force, &format!("{UNASSIGNED} f(10, {{}})"));
    assert!(stats.speculated >= 1, "{stats:?}");
    assert_eq!(stats.deopts, 0, "{stats:?}");
}

// TASK-103.34: a register compiled code took without a guard (a local not
// assigned yet, written before read on every normal path) must reach a
// handler, and the frame the interpreter gets back, as the value it had:
// never as that value's payload bytes boxed as a number.

/// The function of the review's reproduction (docs/jit-review-2026-11.md):
/// `x` is read by the loop's arithmetic, so it is guessed to be a number;
/// it is assigned after the instruction that throws, and the handler reads
/// it.
const UNASSIGNED: &str = "function f(n, o) {
    var x;
    var y = 0;
    try {
        for (var i = 0; i < n; i++) {
            o.p;
            x = i;
            y = x + 1;
        }
    } catch (e) {
        return typeof x + ':' + x;
    }
    return 'no throw ' + y;
}";

/// `src` in one mode, checked against `expected`.
fn in_mode(mode: JitMode, src: &str, expected: &str) -> JitStats {
    let (out, stats) = run(mode, src);
    assert_eq!(out, expected, "{mode:?}: {src}");
    stats
}

#[test]
fn an_unassigned_local_is_undefined_in_a_handler_after_a_throw_in_a_speculated_loop() {
    same(&format!("{UNASSIGNED} f(10, null)"), "undefined:undefined");
}

#[test]
fn the_reviews_first_script_prints_the_same_in_modes_on_and_force() {
    // `lead2.js` of the review: a throw on the first iteration, a run
    // without one, and the throw again.
    let src = format!("{UNASSIGNED} [f(10, null), f(10, {{}}), f(10, null)].join(' | ')");
    let expected = "undefined:undefined | no throw 10 | undefined:undefined";
    in_mode(JitMode::Off, &src, expected);
    in_mode(JitMode::Force, &src, expected);
    in_mode(JitMode::On, &src, expected);
}

#[test]
fn the_reviews_tiered_script_prints_the_same_in_modes_on_and_force() {
    // The warm-up compiles `f` in mode `On` from inside its loop, where `x`
    // holds a number; the calls after it enter at the function's start,
    // where it holds nothing yet.
    let src = format!(
        "{UNASSIGNED}
         for (var k = 0; k < 3000; k++) f(50, {{}});
         var seen = {{}};
         for (var k = 0; k < 20; k++) {{ var r = f(50, null); seen[r] = (seen[r] || 0) + 1; }}
         JSON.stringify(seen)"
    );
    let expected = r#"{"undefined:undefined":20}"#;
    in_mode(JitMode::Off, &src, expected);
    let stats = in_mode(JitMode::On, &src, expected);
    assert!(stats.compiled >= 1, "the warm-up compiled f: {stats:?}");
    in_mode(JitMode::Force, &src, expected);
}

#[test]
fn an_unassigned_local_is_undefined_in_a_handler_after_a_throw_from_a_callee() {
    // The throw comes out of a call: like the property load above, an
    // instruction whose register reads compiled code knows.
    same(
        "function boom(o) { return o.p; }
         function f(n, o) {
             var x; var y = 0;
             try { for (var i = 0; i < n; i++) { boom(o); x = i; y = x + 1; } }
             catch (e) { return typeof x + ':' + x; }
             return 'no throw ' + y;
         }
         [f(10, null), f(10, {}), f(10, null)].join(' | ')",
        "undefined:undefined | no throw 10 | undefined:undefined",
    );
    // And once the caller and the callee are both hot.
    in_mode(
        JitMode::On,
        "function boom(o) { return o.p; }
         function f(n, o) {
             var x; var y = 0;
             try { for (var i = 0; i < n; i++) { boom(o); x = i; y = x + 1; } }
             catch (e) { return typeof x + ':' + x; }
             return 'no throw ' + y;
         }
         for (var k = 0; k < 3000; k++) f(50, {});
         var seen = {};
         for (var k = 0; k < 20; k++) { var r = f(50, null); seen[r] = (seen[r] || 0) + 1; }
         JSON.stringify(seen)",
        r#"{"undefined:undefined":20}"#,
    );
}

#[test]
fn a_local_assigned_between_two_throwing_instructions_is_read_right_by_the_handler() {
    // The handler is reached from before the assignment and from after it.
    same(
        "function f(n, o) {
             var x; var y = 0;
             try { for (var i = 0; i < n; i++) { o[0].p; x = i * 2; o[i].p; y = x + 1; } }
             catch (e) { return typeof x + ':' + x + ':' + y; }
             return 'no throw ' + y;
         }
         var ok = [{}, {}, {}, {}];
         [f(4, ok), f(4, [null]), f(4, [{}, null]), f(4, [{}, {}, {}, null]), f(4, [null])].join(' | ')",
        "no throw 7 | undefined:undefined:0 | number:2:1 | number:6:5 | undefined:undefined:0",
    );
}

#[test]
fn a_throw_after_a_side_exit_reads_the_frame_the_exit_wrote() {
    // The review's unreproduced variant: a guard fails (`s` becomes a
    // string) while `x` is still unassigned, the interpreter goes on from
    // the materialised frame, and a throw there reaches the handler.
    same(
        "function f(n, o, a, t) {
             var x; var y = 0; var s = 0;
             try { for (var i = 0; i < n; i++) { s = s + a[i]; if (i == t) o.p; x = i; y = x + 1; } }
             catch (e) { return typeof x + ':' + x + ':' + s; }
             return 'no throw ' + s + ':' + y;
         }
         var nums = [1, 2, 3, 4], strs = ['a', 'b', 'c', 'd'];
         [f(4, null, strs, 0), f(4, {}, nums, 9), f(4, null, nums, 2), f(4, null, strs, 0)].join(' | ')",
        "undefined:undefined:0a | no throw 10:4 | number:1:6 | undefined:undefined:0a",
    );
}
