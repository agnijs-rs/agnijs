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
    // Two guesses fail, one run each: `x` (the string), then `s` (the sum
    // becomes a string when the recompiled code adds the boxed `x`).
    assert_eq!(stats.deopts, 2, "{stats:?}");
    assert!(stats.compiled >= 3, "recompiled twice: {stats:?}");
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
    assert!(stats.deopts >= 5, "{stats:?}");
    assert!(stats.compiled >= 2, "{stats:?}");
}
