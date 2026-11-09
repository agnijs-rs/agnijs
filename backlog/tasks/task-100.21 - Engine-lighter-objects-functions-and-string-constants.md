---
id: TASK-100.21
title: 'Engine: lighter objects, functions and string constants'
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
ordinal: 255000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The engine allocates far more than QuickJS-NG per object. Every object is a 120-byte ObjectData with a separate `slots: Vec<PropSlot>` (32 bytes per slot) and a separate elements Vec. `create_function` allocates the object, a Box<FunctionData>, the slots Vec, the length and name properties, a name string and (for constructors) a prototype object: 28% of closures, 6.6% of rxjs-higher-order, 4.2% of template-mount. `LdaConst` of a string constant calls alloc_str_utf16 every time it runs (per iteration: string-concat 400 K, prop-write-alloc 300 K, map-set 200 K, method-calls 150 K). The system allocator is 30 to 40% self time in promise-chain, async-await, json-parse and regex, 27% in closures and 13 to 20% in the Gable set.

Reproduction (ES module, 1 M iterations, Gable vs QuickJS-NG ms): closure creation `const g = () => i` 456 vs 234; object literal `{ a: i, b: i }` 389 vs 189; a loop reading `"abc".length` 284 vs 53.

Benchmarks to prove it: closures, prop-write-alloc, string-concat, method-calls (micro) and rxjs-higher-order, template-mount, signal-create (Gable).

A faster global allocator in the host binaries (gable-harness, bench) is a separate embedder decision; measure it here if useful, but the engine crates must not set a global allocator.

Findings: docs/js-engine-performance-findings.md, finding 6. Keep the engine crates Gable-agnostic (ADR 0016); the agnijs-* rename is TASK-100.12, so use the current crate names. Measure before and after with the same binaries and machine; micro-benchmarks via `native/engine/target/release/bench.exe run --engine gable --engine quickjs` and `bench compare`, Gable benchmarks via `node native/engine-harness/js/run-bench.mjs --engine gable --engine quickjs`. The sampling profiler is native/tools/winprof (see the findings doc, Method).

Model assignment (maintainer, 2026-11-08, via TASK-100.14): Fable.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Loading a string constant does not allocate a new string each time it executes
- [ ] #2 Creating a function allocates at most two Rust heap blocks in the common case; prototype, length and name stay observable exactly as before (property order, attributes, test262)
- [ ] #3 Small objects keep their first few property slots inline, without a separate slots allocation
- [ ] #4 closures and prop-write-alloc are at least 1.3x faster than at the start of the task
- [ ] #5 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [ ] #6 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->
