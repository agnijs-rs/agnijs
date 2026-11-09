---
id: TASK-100.28
title: 'Engine: faster bytecode loading and lazy debug-info decoding'
status: To Do
assignee: []
created_date: '2026-11-09T03:19:00Z'
labels:
  - ready-for-agent
dependencies: []
documentation:
  - docs/js-engine-performance-findings.md
parent_task_id: TASK-100
priority: low
type: enhancement
ordinal: 262000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
bytecode-load is 5.28x QuickJS-NG (4.48 ms against 0.85 ms) and decode_bytecode is 66% of it. `(0..n).map(..).collect::<Result<Vec<_>>>()` in serialize.rs cannot reserve capacity (the Result adapter reports a lower bound of 0), so Vec growth and RtlReAllocateHeap alone are 15%. Decoding and verifying the M8 debug info (get_debug_info plus verify_debug_info) is 3.4%, the 4 to 12% slowdown TASK-100.9 measured; ADR 0010's addendum names lazy decoding as the next step.

Reproduction: bytecode-load in the micro-benchmarks; profile it with winprof and read get_block and RawVec growth.

Benchmarks to prove it: bytecode-load (micro), and the test262 `--precompile` run time.

Findings: docs/js-engine-performance-findings.md, finding 9e. Keep the engine crates Gable-agnostic (ADR 0016); the agnijs-* rename is TASK-100.12, so use the current crate names. Measure before and after with the same binaries and machine; micro-benchmarks via `native/engine/target/release/bench.exe run --engine gable --engine quickjs` and `bench compare`, Gable benchmarks via `node native/engine-harness/js/run-bench.mjs --engine gable --engine quickjs`. The sampling profiler is native/tools/winprof (see the findings doc, Method).

Model assignment (maintainer, 2026-11-08, via TASK-100.14): Fable.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Decoding preallocates every vector whose length is in the payload
- [ ] #2 Debug info is decoded on first use (debugger, stack trace line lookup) instead of at load, and its verification still rejects malformed payloads before use, with tests
- [ ] #3 bytecode-load is at least 1.3x faster than at the start of the task
- [ ] #4 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [ ] #5 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->
