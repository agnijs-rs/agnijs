---
id: TASK-100.27
title: 'Engine: make shape transitions linear (realm creation is quadratic)'
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
ordinal: 261000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Adding a property to a shape is O(n) in the shape's size, so building an object with n properties is O(n^2). `shape_add` (object.rs) clones the parent's whole key list and rebuilds a HashMap index of every key for each new transition. Realm creation adds dozens of methods to each built-in prototype: Heap::shape_add is 35% inclusive of engine-startup, which is 1.06 ms against 0.16 ms on QuickJS-NG (6.65x). Object literals and classes with many members pay the same cost at run time. Heap and realm snapshots (TASK-101.2) are the long-term startup answer; this is the cheap fix that also helps run time.

Reproduction: engine-startup in the micro-benchmarks; profile it with winprof and read shape_add's inclusive share.

Benchmarks to prove it: engine-startup (micro), ui-bundle-startup (Gable).

Findings: docs/js-engine-performance-findings.md, finding 9d. Keep the engine crates Gable-agnostic (ADR 0016); the agnijs-* rename is TASK-100.12, so use the current crate names. Measure before and after with the same binaries and machine; micro-benchmarks via `native/engine/target/release/bench.exe run --engine gable --engine quickjs` and `bench compare`, Gable benchmarks via `node native/engine-harness/js/run-bench.mjs --engine gable --engine quickjs`. The sampling profiler is native/tools/winprof (see the findings doc, Method).

Model assignment (maintainer, 2026-11-08, via TASK-100.14): Fable.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Adding a property to a shape costs amortised O(1) time and memory (shared key storage, index built lazily or only above the linear-search size), with a test over an object with a few hundred properties
- [ ] #2 engine-startup is at least 1.3x faster than at the start of the task
- [ ] #3 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [ ] #4 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->
