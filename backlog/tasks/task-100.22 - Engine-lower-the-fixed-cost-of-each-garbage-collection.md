---
id: TASK-100.22
title: 'Engine: lower the fixed cost of each garbage collection'
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
ordinal: 256000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
A collection costs about 4 ms even when only 400 KB is live. prop-write-alloc: 12 collections, 47.8 ms (14% of the run); closures: 7, 36 ms (17%); string-concat: 6, 23 ms (16%); promise-chain: 5 collections over 6 iterations, 109 ms with 22.8 MB live. Sweeping is 60 to 80% RtlFreeHeap because each dead ObjectData drops its own Vecs, plus NtFreeVirtualMemory; the root scan visits every key of every shape. In the Gable set, collect is 9.9% of list-diff and 13.2% of template-mount even though collections happen only at job boundaries today. Once TASK-100.16 lets the collector run during execution, collections become more frequent, so this cost grows.

Reproduction: run prop-write-alloc with a temporary counter around Vm::collect (count and nanoseconds), or profile it with winprof and read the Vm::collect inclusive share.

Benchmarks to prove it: prop-write-alloc, closures, string-concat, promise-chain (micro) and list-diff, template-mount (Gable).

Findings: docs/js-engine-performance-findings.md, finding 7. Keep the engine crates Gable-agnostic (ADR 0016); the agnijs-* rename is TASK-100.12, so use the current crate names. Measure before and after with the same binaries and machine; micro-benchmarks via `native/engine/target/release/bench.exe run --engine gable --engine quickjs` and `bench compare`, Gable benchmarks via `node native/engine-harness/js/run-bench.mjs --engine gable --engine quickjs`. The sampling profiler is native/tools/winprof (see the findings doc, Method).

Model assignment (maintainer, 2026-11-08, via TASK-100.14): Fable.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A collection with under 1 MB live takes under 1 ms on the reference machine, measured on prop-write-alloc
- [ ] #2 Shapes are traced from live objects (or otherwise no longer scanned in full as roots), with a test that unreachable shapes are freed
- [ ] #3 list-diff and template-mount spend at least 40% less time in Vm::collect than at the start of the task
- [ ] #4 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [ ] #5 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->
