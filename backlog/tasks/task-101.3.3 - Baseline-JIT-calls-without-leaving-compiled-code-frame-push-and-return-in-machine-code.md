---
id: TASK-101.3.3
title: >-
  Baseline JIT: calls without leaving compiled code (frame push and return in
  machine code)
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-10T11:19:00Z'
updated_date: '2026-11-10T15:06:00Z'
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
- [ ] #1 closures, calls-recursive and method-calls are faster with AGNIJS_JIT=force than interpreted, measured with agnijs-js-bench
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
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Forced calls to already-compiled functions no longer leave compiled code: the callee's machine code is entered inside the call out (up to 64 nested levels), with no deoptimization or GC maps (I1-I3). The Call entry and the light LdaGlobal/LdaEnv/StaEnv/LdaCallee entry are dedicated, LdaThis is inline, and a named-access miss probes its cache once. Forced: method-calls 1.25 to 0.89, closures 1.33 to 1.07, calls-recursive 1.30 to 1.07, Gable rxjs/signal/template benches 1.16-1.38 to 1.11-1.27. Mode on is within noise everywhere. A 6% feature-off regression on prop-write-alloc, from padding left by the OptObj prototype, is fixed. All gates are green (test262 x18, GC stress, VM suite, Gable suites). AC#1 is only partly met and AC#2 is not met. The remaining cost is per-call Rust glue (Ctx setup per call, trampolines, the shared frame push and pop), documented in the ADR 0020 addendum. It needs the frame push and return in machine code or one Ctx per nest, not TASK-101.3.1. The task stays In Progress pending a decision on that follow-up.
<!-- SECTION:FINAL_SUMMARY:END -->
