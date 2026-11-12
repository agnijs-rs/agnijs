---
id: TASK-101.3.3
title: >-
  Baseline JIT: calls without leaving compiled code (frame push and return in
  machine code)
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-10T11:19:00Z'
updated_date: '2026-11-12T13:50:00Z'
labels:
  - needs-triage
dependencies: []
references:
  - docs/adr/0020-baseline-jit-on-cranelift-codegen-with-no-speculation.md
  - docs/design/jit-deopt-and-gc-maps.md
parent_task_id: TASK-101.3
ordinal: 287000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Measured in TASK-101.3.2 (ADR 0020 addendum): inline IC hits make property-heavy loops 1.3 to 1.4 times faster, but every call still leaves compiled code (Exit::Frame), goes through Vm::jit_run_slow and its J7 polls, and the return re-enters the caller with the frame reloaded from memory. That round trip costs about 30 ns, against about 11 ns for a call out, so closures, calls-recursive and method-calls lose 25 to 33% when forced, and Gable's benches (rxjs, signals, templates) lose 16 to 38%. Mode on can only leave such code to the interpreter, where it breaks even. Two options to weigh: (a) without native recursion, push the callee's interpreter frame in machine code (same frame format, J1) and hand over through a tight trampoline that enters the callee's compiled code without the slow-path checks, with the return path mirrored; this needs no deoptimization or GC maps if nothing stays in machine registers across the call. (b) Direct calls between compiled functions on the native stack, which need TASK-101.3.1 (deopt and GC maps) and the native-depth and birth-serial accounting reworked. Also worth weighing: compile time is about 25 us per bytecode instruction once IC paths are expanded inline, and out-of-line IC stubs would cut it.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 closures, calls-recursive and method-calls are faster with AGNIJS_JIT=force than interpreted, measured with agnijs-js-bench
- [ ] #2 Gable's rxjs and signal benches are no slower with AGNIJS_JIT=force than interpreted
- [x] #3 jit_differential, the VM suite and test262 pass with AGNIJS_JIT=force, including under GC stress
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Nested entry without native-stack speculation: when an instruction run by Runtime::slow (a call, construct, getter, setter) pushes exactly one bytecode frame whose block has compiled code, the runtime enters the callee's compiled code from inside the call out (bounded nesting depth), pushing the frame the interpreter's way (push_function_frame), with the function-entry poll; on the callee's Return it pops the frame (do_return), polls, and resumes the caller's compiled code at pc+1 directly (Resume::Next), with no exit to Vm::jit_run. Invariants I1-I3 hold: the caller has flushed everything to its interpreter frame, so no GC map is needed and nothing deoptimizes; any other exit of the callee (Frame, Fault, Interp, Done) unwinds every nested machine frame back to Vm::jit_run, whose frames are already interpreter-valid.
2. Debugger attached: never nest (J9); attention word: service the safe point as jit_run_slow does.
3. Tests (jit_differential): direct-call stat under force for fib/closures/methods; exceptions thrown in a nested callee caught by a compiled caller and unwinding several compiled frames; deep recursion past the nesting cap; GC stress; interrupts; debugger.
4. Cut the per-call cost (dedicated call path, skip redundant checks), measure per workload, alternating binaries median of 5.
5. Mode on: let loop-free callees compile when called from compiled code if profitable; retune ENTRY_SHARE/judge so mode on never loses.
6. Feature-off interpreter: same-path builds of main vs head on prop-write-alloc and promise-chain; fix if real.
7. Gates: test262 (default, --precompile, intl402, RegExp x3) off/on/force, gc-stress subset, Gable suites; ADR 0020 addendum; notes.

Revision of step 5: loop-free callees stay uncompiled in mode on, because forced calls still lose to the interpreter (see notes). Mode on is unchanged and within noise.

Second run (frame push and return in machine code). Baseline at main 1d172aa, same binary, force/off: closures 1.27, calls-recursive 1.25, method-calls 1.05 (the nested entry lost ground when TASK-101.3.1 landed after it).

8. CallLayout: the VM describes its frame stack, register file and function objects to the JIT by address differences (as HeapLayout); the JIT crate probes Vec and Rc words in exec.rs. Each compiled block gets a table of call sites (one per Call), a monomorphic cache keyed by the callee's code block and filled by the runtime on a miss with the callee's machine entry and a template Frame the VM built in safe code.

9. Call in machine code on a cache hit: guards (function object, same code and realm, room in both stacks, depth bound, no poll or attention due, fresh inline caches), then the frame is pushed by copying the template and patching base, this, callee, env and module, registers cleared, arguments copied, and the callee's machine code called directly on the native stack with its own context in the caller's machine frame. Only callees that keep no register in machine state are called this way (no GC map needed for them). Return pops the frame in machine code. Any other exit unwinds each machine frame through a helper that materialises it, as the nested entry does.

10. Split Ctx into per-frame and per-run (shared) parts; JitRt becomes per-run and finds its frame at the top of the frame stack.

11. Tests: direct-call stat, exceptions through direct frames, deep recursion past the bound, polymorphic and megamorphic sites, sloppy this, arity mismatch, GC with frequent collections, interrupts, debugger attach, realm change; Miri for the new exec.rs probes.

12. Measure the three call workloads and the Gable set; then inline global and environment loads and cheaper generic call outs as the profile says; mode on policy only if forced calls win.

13. Gates as listed by the parent, ADR 0020 addendum, design doc, notes.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Design (ADR 0020 addendum, TASK-101.3.3):

- Nested entry. When a call out pushes exactly one frame and the callee's block is already compiled at that pc, JitRt::enter_callee runs the callee's machine code inside the call out, at most 64 levels deep. On Return it runs do_return, polls, and hands the caller Resume::Next, so the caller continues in machine code. Any other exit unwinds to Vm::jit_run.
- Invariants I1 to I3 hold, so there is no deoptimization and no GC map.
- With the debugger attached, or when the attention word is set, the runtime takes the full after() path.
- Dedicated entries Ctx::call and Ctx::fast. The fast entry (LdaGlobal, LdaEnv, StaEnv, LdaCallee) skips the window, heap and counter reload.
- LdaThis compiles inline, from Ctx::this.
- A GetNamed or SetNamed miss probes the cache once instead of three times.

Measured, median of 5, alternating rounds, same binary, force/off:

| Workload | Before | After |
|---|---|---|
| closures | 1.33 | 1.07 |
| calls-recursive | 1.30 | 1.07 |
| method-calls | 1.25 | 0.89 |
| rxjs-operators | 1.38 | 1.27 |
| rxjs-higher-order | 1.27 | 1.18 |
| rxjs-subject-fanout | 1.30 | 1.11 |
| signal-churn | 1.26 | 1.14 |
| signal-create | 1.30 | 1.21 |

- Mode on is within noise everywhere: micro 0.99 to 1.02, Gable 0.986 to 1.010.
- AC#1 is met only for method-calls. AC#2 is not met.
- The remaining cost is per-call glue, not speculation:
  - Ctx setup in enter_callee and exec::enter, about 22% on rxjs-operators;
  - the trampolines;
  - the shared push_function_frame, do_return and regs.resize.
- Closing the gap needs the frame push and return in machine code, or one Ctx per nest instead of one per call. Neither needs TASK-101.3.1. The Vec/Rc Frame representation makes the first hard from Cranelift.

Feature-off interpreter:
- promise-chain is noise.
- prop-write-alloc was really about 6% slower than pre-JIT main. Same-path builds in two checkout paths showed it, and a bisect put it at a96b628 (proto as OptObj left four bytes of padding in ObjectData).
- Fixed with an explicit zeroed u32, commit 332360a. It now measures 0.98 of the pre-JIT build.

Gates at 332360a plus the ADR addendum, each run once:

- test262 with the feature off, with AGNIJS_JIT=on and with force: 18 runs (default and --precompile 42231/42231, intl402 981/981, the RegExp suites in auto, backtracking-only and linear-when-eligible), all exit 0 with 0 regressions.
- Forced GC stress (AGNIJS_GC_STRESS=1, AGNIJS_JIT=force): jit_differential 20/20, gc_safe_points 2/2. jit_inline_cache 14/14 under GC stress.
- The VM suite passes forced and with the feature off.
- Gable suites through the path-patched harness: 759 passed and 1 known yjs timing failure in off, on and force. The harness manifest, lockfile and binary are restored.

Micro, same binary, on/off:
- loop-arith 0.37; prop-mono, prop-poly and loop-array-index 0.57 to 0.59;
- regex-tokenize 0.91;
- everything else 0.99 to 1.02.

AC status:
- #3 is met.
- #1 is met only for method-calls (0.89). closures and calls-recursive are 1.07 forced.
- #2 is not met: rxjs and signal benches are 1.11 to 1.27 forced.
- See the ADR addendum for why: per-call glue in Rust, not speculation.

Second run: frame push and return in machine code (plan steps 8 to 13). Design is in the ADR 0020 addendum "frame push and return in machine code" and docs/design/jit-deopt-and-gc-maps.md section 9.

Design, in short:
- CallLayout describes the frame stack, register file and function objects by address differences. The Vec, Rc and Option probes are in exec.rs (the only unsafe file) and run under Miri.
- A callee table per VM (256 buckets of 4) replaces per-site caches: code block to machine entry, inline-cache table, register counts, realm and a frame image the VM builds in safe code.
- One call thunk per layout does guards, frame push from the image, argument copy, a direct call on the native stack, and the pop. A failed guard calls through the runtime; a callee that leaves compiled code makes each machine frame below it leave, materialised.
- Only callees that keep nothing in machine state are called directly, so no GC map and no deoptimization (invariants I4 and I5 beside I1 to I3).
- Ctx is split into Shared (per run) and Ctx (per frame). The 64-level bound became a floor in bytes (JIT_STACK, 96 KB release), and the floor is never past the host's stack budget (TASK-103.27).
- A callee without compiled code is interpreted inside the call out instead of unwinding every machine frame. Base class constructors compile.
- Inline: environment slots, global bindings (heap global stamp), equality without heap or conversion.
- Mode on: a loop-free function compiles when compiled code or its own recursion calls it (Tier::DEMAND), and each block's runs are judged cost against work (Tier::DIRECT).

Measured, i9-9900KF, release, one path, median of 5 alternating rounds, ratio over the same binary with the JIT off. Before is main 1d172aa.

agnijs-js-bench (force before, force now, on before, on now):
- closures 1.26, 0.69, 1.00, 0.78
- calls-recursive 1.23, 0.38, 0.97, 0.37
- method-calls 1.04, 0.77, 0.99, 1.02
- loop-arith 0.32, 0.32, 0.31, 0.32
- loop-array-index 0.56, 0.26, 0.54, 0.26
- prop-monomorphic 0.59, 0.31, 0.58, 0.30
- prop-polymorphic 0.53, 0.27, 0.52, 0.28
- prop-write-alloc 1.24, 1.33, 0.99, 0.99
- string-concat 1.26, 1.30, 1.02, 0.99
- string-methods 1.02, 1.03, 1.00, 1.00
- array-methods 1.14, 1.21, 1.00, 0.98
- map-set 1.05, 1.11, 0.97, 1.00
- regex 1.03, 1.03, 0.99, 1.05
- regex-tokenize 1.03, 0.96, 1.02, 0.94
- regex-long-input 1.10, 1.25, 0.95, 1.02
- json-stringify 1.01, 1.01, 1.00, 1.00
- json-parse 1.04, 1.03, 1.03, 1.01
- promise-chain 1.10, 1.12, 1.00, 1.01
- async-await 1.00, 1.04, 0.99, 1.00
- engine-startup 1.38, 1.37, 1.02, 1.01
- snapshot-startup 1.51, 1.46, 0.94, 1.05 (sub-millisecond, noisy)
- on only: compile-source 1.00 to 1.00, bytecode-load 0.98 to 1.01, bundle-cold 0.98 to 1.00, bundle-snapshot 1.00 to 1.02

Gable benches, harness of Gable 7cd58eb built out of tree against this tree (off ms, on, force, on before, force before):
- rxjs-operators 171.6, 0.99, 0.78, 1.00, 1.42
- rxjs-higher-order 256.8, 1.02, 1.02, 1.01, 1.33
- rxjs-subject-fanout 117.7, 0.74, 0.74, 1.00, 1.25
- signal-churn 456.1, 1.01, 1.03, 1.00, 1.29
- signal-create 86.1, 0.99, 1.00, 1.02, 1.33
- template-mount 71.7, 1.02, 1.19, 1.00, 1.32
- template-update 39.4, 1.01, 1.15, 1.00, 1.39
- list-diff 832.7, 1.03, 1.08, 1.00, 1.21
- ui-bundle-startup 96.7, 1.00, 10.9, 1.00, 10.2
- geomean: on 0.97 (before 1.00), force 1.29 (before 1.65)
- A 10-round forced check: rxjs-higher-order 1.03, signal-churn 1.03, signal-create 1.02, same binary against itself 1.00.

Interpreter with the feature off: 25 workloads, 5 rounds, head over 1d172aa built from one path, geomean 1.002 (control 1.003).

Gates, on the final tree rebased onto main fa02ecc:
- test262, 0 regressions in every run: feature off default, --precompile and --snapshot 43192/43192, --only-intl402 981/981, QuickJS-NG 43073 pass and 84 fail (the baseline); AGNIJS_JIT=force default, --precompile and --snapshot 43192/43192, force intl402 981/981; AGNIJS_JIT=on 43192/43192 and intl402 981/981; GC-stress slice (AGNIJS_GC_STRESS=16, language statements, expressions, function-code, arguments-object) force and on 20586/20586.
- VM suite with the jit feature: 349 passed plain and 349 with AGNIJS_JIT=force (debugger, fuel, interrupt, gc and the new stack_budget tests among them); 289 with the feature off.
- jit_calls 18, jit_differential 21, jit_deopt 6, jit_inline_cache 14, gc_safe_points 2 under AGNIJS_GC_STRESS=1, tiered and forced (release): all pass.
- JIT crate 26 tests; Miri on exec::tests and calls::tests 8/8.
- jit_workloads (release) passes; capi fuzz 5/5; instr_count 3/3; regex and compiler crates 94 tests.
- cargo check --workspace --all-targets clean; wasm32 check of the JIT crate and the VM passes; clippy -D warnings clean on the JIT crate, and no warning on a line this work touched in the VM; rustfmt on touched hunks only.
- unsafe is still only in crates/agnijs-js-jit/src/exec.rs.

Found on the way:
- Three statistics bounds in jit_calls and jit_inline_cache do not hold under AGNIJS_GC_STRESS (every return is a safe point there); they are skipped under stress as the older bound in jit_inline_cache is. Values are still compared.
- The callee table's size assertion failed the wasm32 build of the JIT crate; it is now only made where there is a code generator.

AC status after this run:
- #1 met: closures 0.69, calls-recursive 0.38, method-calls 0.77 forced.
- #2 not fully met: rxjs-operators 0.78 and rxjs-subject-fanout 0.74 forced, signal-create 1.00, but rxjs-higher-order and signal-churn are 2 to 3% slower forced (from 1.33 and 1.29).
- #3 holds.

What is left for #2 and beyond (ADR addendum, "What this did not reach"):
- new, getters, setters and Function.prototype.call/apply are not direct calls yet;
- megamorphic GetNamed is 78% of signal-churn's call outs;
- typeof x === "..." is three call outs;
- mode on does not compile groups of functions that call each other from interpreted callers (rxjs-operators stays at 0.99 tiered), and regex is 5% slower tiered: both need call-target feedback;
- forced compile time (compile-source 8.4 s to 8.9 s).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
A call of a compiled function is now made by machine code: a call thunk pushes the callee's interpreter frame from an image the VM built, calls the callee's machine code on the native stack and pops the frame, with a per-VM callee table instead of per-site caches. The frame stack stays the interpreter's and nothing speculates (invariants I4 and I5 beside I1 to I3; ADR 0020 addendum and design doc section 9). Environment slots, global bindings and heap-free equality are inline, a callee without compiled code is interpreted inside the call out, and mode on compiles loop-free functions that compiled code calls.

Forced, over the interpreter, before to now: closures 1.26 to 0.69, calls-recursive 1.23 to 0.38, method-calls 1.04 to 0.77; Gable rxjs-operators 1.42 to 0.78, rxjs-subject-fanout 1.25 to 0.74, signal-create 1.33 to 1.00, rxjs-higher-order 1.33 to 1.02, signal-churn 1.29 to 1.03 (forced geomean of Gable's nine 1.65 to 1.29). Mode on: calls-recursive 0.37, closures 0.78, rxjs-subject-fanout 0.74, the global-reading loops halved again; regex is 5% slower tiered (documented). The interpreter without the feature is unchanged (geomean 1.002).

All gates pass with 0 regressions (test262 in 13 configurations, VM suite forced, GC stress, Miri, QuickJS-NG baseline); numbers are in the notes.

AC#1 and AC#3 are met. AC#2 is partly met: two of Gable's five rxjs and signal benches are 22 to 26% faster forced and one is level, but rxjs-higher-order and signal-churn are still 2 to 3% slower forced. Closing that needs direct calls for new, getters and call/apply, and a megamorphic property cache (78% of signal-churn's call outs); see the addendum's "What this did not reach". The task stays In Progress pending a decision on that follow-up.
<!-- SECTION:FINAL_SUMMARY:END -->
