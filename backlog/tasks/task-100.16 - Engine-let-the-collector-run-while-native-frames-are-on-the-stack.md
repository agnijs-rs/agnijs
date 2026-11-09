---
id: TASK-100.16
title: 'Engine: let the collector run while native frames are on the stack'
status: To Do
assignee: []
created_date: '2026-11-09T03:18:00Z'
labels:
  - ready-for-agent
dependencies: []
documentation:
  - docs/js-engine-performance-findings.md
parent_task_id: TASK-100
priority: high
type: enhancement
ordinal: 250000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The collector never runs during embedded execution. `Vm::maybe_collect` returns at once when `native_depth != 0`, and `in_native` raises it for every native function and host entry: `engine::call`, module evaluation (`execute_module` uses `call_value`), promise jobs and every native that calls back (`Function.prototype.call`, `Array.prototype.map` and so on). Collections happen only between jobs and between host calls, so one long synchronous call keeps all of its garbage. This is the whole peak-memory gap in the M9 set: rxjs-higher-order 124 MiB against 6.7 MiB on QuickJS-NG, list-diff 238 against 41, template-mount 52 against 18, signal-create 46 against 8. It also makes the run loop's slow branch (memory-limit check) run before every instruction once the threshold is passed inside a deferred region.

Reproduction (ES module, `gable-harness run --engine gable|quickjs`, measure peak working set): `function work(){ let keep = 0; for (let i = 0; i < 2000000; i++) { const o = { a: i, b: [i, i + 1] }; keep += o.b.length; } return keep; } console.log(work());` peaks at 900.7 MiB on the Gable engine and 5.4 MiB on QuickJS-NG; wrapping the call in `[0].forEach(() => ...)` gives the same numbers.

Benchmark to prove it: peak RSS of rxjs-higher-order, list-diff, template-mount and signal-create in run-bench.mjs.

Collecting under a native frame needs every value a native holds in Rust locals to be rooted (a root or handle stack, or VM-visible slots). A missed root is a use-after-free, so this is correctness-critical. TASK-100.19 (calls) removes the native frame from Function.prototype.call/apply and host entry, which takes away the most common case first; doing it first is recommended but not required.

Findings: docs/js-engine-performance-findings.md, finding 1. Keep the engine crates Gable-agnostic (ADR 0016); the agnijs-* rename is TASK-100.12, so use the current crate names. Measure before and after with the same binaries and machine; micro-benchmarks via `native/engine/target/release/bench.exe run --engine gable --engine quickjs` and `bench compare`, Gable benchmarks via `node native/engine-harness/js/run-bench.mjs --engine gable --engine quickjs`. The sampling profiler is native/tools/winprof (see the findings doc, Method).

Model assignment (maintainer, 2026-11-08, via TASK-100.14): Fable.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 The collector can run at interpreter safe points while one or more native frames are on the stack, including module evaluation, host calls, promise jobs and native callbacks
- [ ] #2 The reproduction above peaks below 32 MiB of working set, both at module top level and inside a forEach callback
- [ ] #3 Peak RSS of rxjs-higher-order and list-diff is within 2x of QuickJS-NG in run-bench.mjs
- [ ] #4 The GC stress mode (collect at every safe point) runs over test262 and the Gable suites with no failures, and a test covers each native that calls back into JavaScript while holding heap values
- [ ] #5 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [ ] #6 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->
