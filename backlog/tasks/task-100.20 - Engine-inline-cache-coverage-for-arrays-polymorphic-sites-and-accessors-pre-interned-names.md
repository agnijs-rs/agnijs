---
id: TASK-100.20
title: >-
  Engine: inline-cache coverage for arrays, polymorphic sites and accessors;
  pre-interned names
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
ordinal: 254000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The inline cache works for plain own data properties but most hot reads miss it. Counters for one iteration: prop-monomorphic 2.0 M hits and 1.0 M misses (all `arr.length`); prop-polymorphic 0 hits, 3.0 M misses (2.0 M shape changes, 1.0 M arrays); loop-array-index 1.5 M misses, all arrays; method-calls 0.75 M hits, 0.90 M misses (0.30 M shape, 0.30 M accessor, 0.30 M array). `interp/ic.rs` is monomorphic and `is_plain` excludes arrays, strings, arguments, typed arrays, namespaces, proxies and dictionary objects. Every miss calls `name_key`, which re-interns the property name (hash plus intern lookup plus a clone of `code.names[i]`): 2 to 5% inclusive across the Gable set, 12 to 14% as Heap::atom in prop-polymorphic. GetKeyed has no dense-array fast path (it goes through to_property_key, get_value, get_prop, exotic_of and own_property).

Reproduction (ES module, 1 M reads, Gable vs QuickJS-NG ms): one shape `.a` 261 vs 57; four shapes 301 vs 57; `arr.length` 217 vs 42; a class getter 351 vs 89.

Benchmarks to prove it: prop-monomorphic, prop-polymorphic, loop-array-index, method-calls (micro; the worst ratios at 4.9x to 6.9x) and signal-churn, rxjs-operators (Gable).

Findings: docs/js-engine-performance-findings.md, finding 5. Keep the engine crates Gable-agnostic (ADR 0016); the agnijs-* rename is TASK-100.12, so use the current crate names. Measure before and after with the same binaries and machine; micro-benchmarks via `native/engine/target/release/bench.exe run --engine gable --engine quickjs` and `bench compare`, Gable benchmarks via `node native/engine-harness/js/run-bench.mjs --engine gable --engine quickjs`. The sampling profiler is native/tools/winprof (see the findings doc, Method).

Model assignment (maintainer, 2026-11-08, via TASK-100.14): Fable.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Property names are interned once per CodeBlock (at compile or load time), so an inline-cache miss does no string hashing for the name
- [ ] #2 Inline caches handle up to four shapes per site, array length, and methods found on the prototype of arrays and strings, with invalidation tests for shape, prototype and property attribute changes
- [ ] #3 Accessor properties found through the cache call the getter or setter without the generic lookup
- [ ] #4 Integer-index reads and writes of dense arrays take a fast path in GetKeyed and SetKeyed
- [ ] #5 prop-polymorphic and loop-array-index are at least 1.5x faster than at the start of the task
- [ ] #6 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [ ] #7 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->
