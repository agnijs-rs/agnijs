---
id: TASK-100.26
title: 'Parser and compiler: cut allocation in the front end'
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
ordinal: 260000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Compiling is allocation-bound. compile-source is 2.30x QuickJS-NG and ui-bundle-startup (the @gable/ui bundle) 2.07x; in ui-bundle-startup, compiling the module graph is 74% of the time (parse 38%, compile 26%). RtlAllocateHeap plus RtlFreeHeap are 36% self time in compile-source and 32% in ui-bundle-startup: boxed AST nodes, Rc<str> names and SipHash HashMaps in scope analysis. A fat-LTO, one-codegen-unit build made compile-source about 15% faster (other workloads did not move), so the release profile is worth checking here too.

Reproduction: profile `bench child --engine gable --workload compile-source` or `gable-harness startup --engine gable native/engine-harness/out/bench/ui-startup.mjs` with winprof and read the allocator share.

Benchmarks to prove it: compile-source (micro) and ui-bundle-startup (Gable).

Findings: docs/js-engine-performance-findings.md, finding 9c. Keep the engine crates Gable-agnostic (ADR 0016); the agnijs-* rename is TASK-100.12, so use the current crate names. Measure before and after with the same binaries and machine; micro-benchmarks via `native/engine/target/release/bench.exe run --engine gable --engine quickjs` and `bench compare`, Gable benchmarks via `node native/engine-harness/js/run-bench.mjs --engine gable --engine quickjs`. The sampling profiler is native/tools/winprof (see the findings doc, Method).

Model assignment (maintainer, 2026-11-08, via TASK-100.14): Fable.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 compile-source and ui-bundle-startup are at least 1.3x faster than at the start of the task
- [ ] #2 The parser's existing tests and the test262 parse-error tests still pass, with identical error messages and positions
- [ ] #3 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [ ] #4 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->
