---
id: TASK-100.24
title: 'Engine: cache global variable lookups'
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
ordinal: 258000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Every global read does three hash lookups in `load_global` (interp/globals.rs): `global_lexicals` (IndexMap<Rc<str>> with SipHash), `self.key(n)` (interning the name) and `get_own_property` on the global object, which is in dictionary mode (another SipHash IndexMap). It is 16 to 26% inclusive in prop-monomorphic, loop-array-index, prop-polymorphic, method-calls and calls-recursive (fib is a global: 656 K loads per run). Bundled Gable code mostly uses module scope, so it is under 2% there, but plain scripts and globals like Math, Object and Array hit it.

Reproduction (ES module, 1 M reads of `globalThis.GG` through the bare name `GG`): 176 ms against 42 ms on QuickJS-NG (an empty loop is 129 vs 37).

Benchmarks to prove it: calls-recursive, prop-monomorphic, loop-array-index (micro).

Findings: docs/js-engine-performance-findings.md, finding 9a. Keep the engine crates Gable-agnostic (ADR 0016); the agnijs-* rename is TASK-100.12, so use the current crate names. Measure before and after with the same binaries and machine; micro-benchmarks via `native/engine/target/release/bench.exe run --engine gable --engine quickjs` and `bench compare`, Gable benchmarks via `node native/engine-harness/js/run-bench.mjs --engine gable --engine quickjs`. The sampling profiler is native/tools/winprof (see the findings doc, Method).

Model assignment (maintainer, 2026-11-08, via TASK-100.14): Fable.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A global read or write at a site whose binding has not changed does no hashing (per-site cache, property cell or equivalent), with invalidation tests for delete, redefinition, accessors and a global lexical shadowing a global property
- [ ] #2 calls-recursive is at least 1.15x faster than at the start of the task
- [ ] #3 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [ ] #4 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->
