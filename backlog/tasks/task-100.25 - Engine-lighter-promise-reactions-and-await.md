---
id: TASK-100.25
title: 'Engine: lighter promise reactions and await'
status: To Do
assignee: []
created_date: '2026-11-09T03:18:00Z'
labels:
  - ready-for-agent
dependencies: []
documentation:
  - docs/js-engine-performance-findings.md
parent_task_id: TASK-100
priority: medium
type: enhancement
ordinal: 259000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Promise and await internals allocate real function objects nobody can observe. new_promise_capability and new_native_closure build resolving functions and await reactions as full closures: 28.6% of async-await. GC is 32 to 34% of async-await and the allocator about 40%. Each pending promise holds about 1.1 KB live; promise-chain peaks at 161.7 MiB against 37.1 MiB on QuickJS-NG and runs 2.80x slower, async-await 3.24x.

Reproduction (ES module): `for (let i = 0; i < 100000; i++) Promise.resolve(i).then(() => c++)` takes 196 ms against 147 ms; the micro-benchmarks below show the larger gap.

Benchmarks to prove it: promise-chain, async-await (micro, time and peak RSS).

Job ordering must stay exactly as specified; test262's promise and async tests are the oracle.

Findings: docs/js-engine-performance-findings.md, finding 9b. Keep the engine crates Gable-agnostic (ADR 0016); the agnijs-* rename is TASK-100.12, so use the current crate names. Measure before and after with the same binaries and machine; micro-benchmarks via `native/engine/target/release/bench.exe run --engine gable --engine quickjs` and `bench compare`, Gable benchmarks via `node native/engine-harness/js/run-bench.mjs --engine gable --engine quickjs`. The sampling profiler is native/tools/winprof (see the findings doc, Method).

Model assignment (maintainer, 2026-11-08, via TASK-100.14): Fable.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Resolving functions and await reactions that script cannot observe are internal records, not function objects; resolving functions that script can observe behave exactly as before
- [ ] #2 promise-chain and async-await are at least 1.4x faster and promise-chain peak RSS at least 30% lower than at the start of the task
- [ ] #3 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [ ] #4 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->
