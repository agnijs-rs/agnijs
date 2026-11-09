---
id: TASK-100.23
title: >-
  Engine: count element, slot and dictionary growth towards collection and the
  memory limit
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
ordinal: 257000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Only fixed sizes count towards `allocated_since_gc`: 160 bytes per object, 64 per environment, 40 plus length per string. Growth of elements, slots and dictionaries never counts, so a heap can grow without triggering a collection, and the memory limit check in the run loop undercounts by the same amount. array-methods ran zero collections over six iterations while heap_bytes reached 81.7 MB across 21 K objects (peak RSS 105.6 MiB against 20.3 MiB on QuickJS-NG). This is what made the collector look "too lazy": the threshold `max(2 x live, 8 MiB)` is reasonable once growth is counted.

Reproduction (classic script, so TASK-100.16 does not mask it): `for (let k = 0; k < 20000; k++) { const a = []; for (let j = 0; j < 1000; j++) a.push(j); }` and compare the collection count and peak RSS; as a module it peaks at 323 MiB against 5.6 MiB.

Benchmarks to prove it: array-methods peak RSS and collection count (micro), list-diff peak RSS (Gable, after TASK-100.16).

Findings: docs/js-engine-performance-findings.md, finding 8. Keep the engine crates Gable-agnostic (ADR 0016); the agnijs-* rename is TASK-100.12, so use the current crate names. Measure before and after with the same binaries and machine; micro-benchmarks via `native/engine/target/release/bench.exe run --engine gable --engine quickjs` and `bench compare`, Gable benchmarks via `node native/engine-harness/js/run-bench.mjs --engine gable --engine quickjs`. The sampling profiler is native/tools/winprof (see the findings doc, Method).

Model assignment (maintainer, 2026-11-08, via TASK-100.14): Fable.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Growth of object elements, property slots, dictionaries and string buffers is added to the allocation counter used for GC triggering and for the memory limit
- [ ] #2 A test shows a loop that only grows arrays triggers collections and hits a configured memory limit with an out-of-memory error instead of growing past it
- [ ] #3 array-methods peak RSS falls to within 2x of QuickJS-NG without a time regression above 5%
- [ ] #4 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [ ] #5 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->
