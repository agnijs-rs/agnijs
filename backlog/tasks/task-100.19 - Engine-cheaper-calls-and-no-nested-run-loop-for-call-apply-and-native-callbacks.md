---
id: TASK-100.19
title: >-
  Engine: cheaper calls and no nested run loop for call, apply and native
  callbacks
status: To Do
assignee: []
created_date: '2026-11-09T03:18:00Z'
updated_date: '2026-11-09T05:53:00Z'
labels:
  - ready-for-agent
dependencies: []
documentation:
  - docs/js-engine-performance-findings.md
parent_task_id: TASK-100
priority: high
type: enhancement
ordinal: 253000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Calls are expensive. The `args_vec!` macro (`interp/step.rs`) copies the arguments into a new `Vec<Value>` for every Call, CallMethod and Construct; in rxjs-operators 107 of the 115 allocation samples under the interpreter come from it, and in calls-recursive the allocator is about 9%. `push_function_frame` is 7 to 13% inclusive across the Gable set (it clones the code Rc, builds a 160-byte Frame, resizes `regs` and copies parameters). A native that calls JavaScript goes `call_object` -> `run_function` -> `in_native` -> a fresh `Vm::run` loop: array-methods does 2.4 M such re-entries and method-calls 300 K (one per getter). `Function.prototype.call` (`proto_call`) is under 94 to 100% of the samples in every Gable benchmark because bundled code calls through it, so all of that code runs nested and under a native frame (which also blocks the collector, TASK-100.16).

Reproduction (ES module, 1 M iterations, Gable vs QuickJS-NG ms): `f(s)` 276 vs 63; `f.call(null, s)` 361 vs 96; `f.apply(null, [s])` 548 vs 197; `Math.abs(1)` 286 vs 80; `arr.forEach` 1 M callbacks 244 vs 45; getter `p.x` 351 vs 89.

Benchmarks to prove it: calls-recursive, method-calls, array-methods, closures (micro) and rxjs-operators, rxjs-subject-fanout, signal-churn (Gable).

Findings: docs/js-engine-performance-findings.md, finding 4. Keep the engine crates Gable-agnostic (ADR 0016); the agnijs-* rename is TASK-100.12, so use the current crate names. Measure before and after with the same binaries and machine; micro-benchmarks via `native/engine/target/release/bench.exe run --engine gable --engine quickjs` and `bench compare`, Gable benchmarks via `node native/engine-harness/js/run-bench.mjs --engine gable --engine quickjs`. The sampling profiler is native/tools/winprof (see the findings doc, Method).

Model assignment (maintainer, 2026-11-08, via TASK-100.14): Fable.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A JavaScript-to-JavaScript call allocates nothing on the Rust heap in the common case (no per-call arguments Vec), checked by a test or counter
- [ ] #2 Function.prototype.call and Function.prototype.apply with a JavaScript target run the target in the current run loop, without a native frame
- [ ] #3 Array iteration methods and accessor calls reach JavaScript callbacks without starting a new Vm::run loop per callback, or the task notes measure why that is not worth it
- [ ] #4 calls-recursive and method-calls are at least 1.3x faster than at the start of the task
- [ ] #5 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [ ] #6 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
JIT-readiness (ADR 0019, TASK-101.1): J1, J8 apply. Make one function the entry into JavaScript for natives, host calls, jobs and call/apply (that is where tier selection will live), and one frame-push path for JS-to-JS calls; today five places call Vm::run. Pass arguments as a slice of the caller's register window. Every frame, however cheap, stays a Frame record in Vm::frames over a Vm::regs window (J1); do not introduce frames that exist only on the Rust stack. Faults stay returned values (J8).
<!-- SECTION:NOTES:END -->
