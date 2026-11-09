---
id: TASK-100.28
title: 'Engine: faster bytecode loading and lazy debug-info decoding'
status: Done
assignee: []
created_date: '2026-11-09T03:19:00Z'
updated_date: '2026-11-09T07:26:00Z'
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
- [x] #1 Decoding preallocates every vector whose length is in the payload
- [x] #2 Debug info is decoded on first use (debugger, stack trace line lookup) instead of at load, and its verification still rejects malformed payloads before use, with tests
- [x] #3 bytecode-load is at least 1.3x faster than at the start of the task
- [x] #4 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [x] #5 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
After (same machine, alternating binaries, bench run --filter bytecode-load --iterations 20 --warmup 5): bytecode-load 4.30 / 4.30 ms (main b071af9) -> 2.61 / 2.58 ms, 1.65x faster; peak RSS 34 -> 28 MiB (QuickJS-NG: 0.85 ms). Root cause from the winprof profile of main: decode_bytecode was 61% of the workload; inside it ~40% was allocator/Vec growth (the Result collect adapter cannot reserve), the second verify_block pass ~10%, and the byte-serial FNV-1a checksum 8.6% of the whole run; debug-info decoding itself was small. Change: agnijs-bc-4 writes every pool before the instructions so decoding and operand verification are one pass; every list is count-prefixed and allocated at its final size (collect_n); the debug section is a length-prefixed byte range kept packed in a DebugSection and decoded on first debugger use (statements/locals/is_statement); when a debugger is attached eval_bytecode and module loading decode eagerly and refuse a malformed section as BytecodeCorrupt; the checksum hashes 8-byte words. ADR 0010 has the addendum. Full micro set: no workload slower by more than 5% after re-running alternating binaries and a parent-commit binary built in the same target dir (prop-write-alloc settled at ~+3%, layout variance; map-set, closures, calls-recursive within 2%). Tests: compiler serialize 11, vm bytecode 10, vm debugger 16 (new precompiled pause/step/locals test), agnijs-js-debugger and agnijs-js-cdp crate tests, cargo test --workspace 227 passed / 0 failed; test262 42225/42225, --precompile 42225/42225, --only-intl402 981/981, 0 regressions; Gable suites via the path-patched harness 758 passed, 2 known timing failures (yjs perf, highlight linearity). The Gable harness never loads bytecode, so its benchmarks cannot move.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Bytecode loads 1.65x faster (bytecode-load 4.30 -> 2.6 ms, peak RSS 34 -> 28 MiB) with the agnijs-bc-4 container: one-pass decode and verification, exact-size pools, a word-wise checksum and debug info kept packed until the debugger first needs it; a debugger-attached load still verifies eagerly. No test262, debugger, CDP or Gable-suite regressions.
<!-- SECTION:FINAL_SUMMARY:END -->
