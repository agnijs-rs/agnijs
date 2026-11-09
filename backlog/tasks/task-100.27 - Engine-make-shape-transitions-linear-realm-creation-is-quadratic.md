---
id: TASK-100.27
title: 'Engine: make shape transitions linear (realm creation is quadratic)'
status: Done
assignee:
  - '@claude-fable'
created_date: '2026-11-09T03:18:00Z'
updated_date: '2026-11-09T06:58:00Z'
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
- [x] #1 Adding a property to a shape costs amortised O(1) time and memory (shared key storage, index built lazily or only above the linear-search size), with a test over an object with a few hundred properties
- [x] #2 engine-startup is at least 1.3x faster than at the start of the task
- [x] #3 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [x] #4 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red: a VM test that builds an object with a few hundred properties (object literal and incremental adds, both below and above DICT_THRESHOLD) and checks every property reads back, plus a unit-level check that adding a key to a shape chain does not copy the parent's keys (shared key table, tip append).
2. Move shape keys out of Shape into heap-owned, append-only key tables (Heap::key_tables, KeyTableId): a Shape holds (table, len, transitions, parent). shape_add appends to the parent's table when the parent is its tip (amortised O(1) time and memory), and forks (copies the prefix) only when a second transition leaves the same parent. The hash index lives on the table and is filled from the 9th key on, so lookup stays a linear scan for small shapes and a shared O(1) map above that; a lookup result is accepted only when it is below the shape's len.
3. Route every reader (Shape::lookup, attrs by slot, own_keys, own_string_keys, make_dictionary, remove_own_property, ic.rs fills, gc.rs key marking which becomes one pass per table instead of per shape) through the heap accessors.
4. Measure engine-startup before (main bench.exe) and after, plus the full micro set; cargo test -p agnijs-js-vm; test262 full, --precompile and intl402, one run each.
5. Record numbers in the notes, ADR 0019 J5 note if layout wording needs it, commit and push to main.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Root cause confirmed in code: Heap::shape_add cloned the parent's whole key Vec and rebuilt a HashMap index of every key for each new transition, so a chain of n adds cost O(n^2) time and allocation; the collector also re-marked every shape's full key list (O(n^2) keys per collection).

Change (crates/agnijs-js-vm/src/object.rs, heap.rs, gc.rs, interp/ic.rs): keys moved out of Shape into heap-owned, append-only KeyTables (Heap::key_tables, KeyTableId). A Shape is (table, len, transitions, parent) and owns the first len entries of its table. shape_add appends to the parent's table when the parent is its tip (amortised O(1) time and memory); a second transition off the same shape forks the table, copying only that prefix. The hash index lives on the table from the 9th key on, so small shapes keep the linear scan and larger ones a shared O(1) map, filtered to positions below the shape's len. Readers go through Heap::shape_lookup / shape_key / shape_keys; the collector marks each table once instead of each shape.

Tests: crates/agnijs-js-vm/tests/shapes.rs: 60 incremental adds create 60 shapes and exactly one table (a second object of that shape creates none); a branch off the middle of a chain forks one table and both chains read every key and reject the other's; 300-property object literal and incremental object (dictionary mode) read back every property and survive deletes. cargo test -p agnijs-js-vm green (81 tests).

engine-startup (bench, agnijs, 1 warmup + 5 iterations, same machine): before (origin/main bc9fcca bench.exe) 0.97 ms, after 0.63 ms (0.65x, 1.54x faster; QuickJS-NG 0.17 ms); peak RSS 7.0 -> 6.2 MiB. Full micro set, after/before: everything 0.97-1.02 except promise-chain 0.71 (62.6 -> 44.5 ms, the same two values TASK-100.23 saw between two builds, so build variance), and four that first read +5-7% (prop-write-alloc 1.07, string-concat 1.07, map-set 1.07, method-calls 1.05). Re-run alternating binaries (2 warmup + 7 iterations, two rounds): prop-write-alloc 360/363 vs 366/365 ms, string-concat 140/144 vs 141/140 ms (noise); map-set and method-calls stayed +5 and +8-12% against the main binary, so a bench.exe was built from the parent commit 2c47fc3 (TASK-100.30 only) and the branch rebuilt: map-set 230/231 vs 235/232 ms (+1%), method-calls 363/362 vs 367/371 ms (+1-2%). The first branch binary was a slow code layout, not this change. No workload regresses by more than 5% against the parent commit.

test262 on this branch, release, one run each: --engine agnijs 42225/42225, --precompile 42225/42225, --only-intl402 981/981, 0 regressions, 0 newly passing.

Gable (run from the Gable repo with native/engine-harness path-patched to the engine before = main bc9fcca and after = this branch, release harness builds, manifests restored afterwards): ui-bundle-startup 145.71 -> 145.80 ms, peak RSS 43.9 -> 43.5 MiB (realm creation is well under a millisecond of a 146 ms bundle load). Gable suites under the agnijs engine on this branch: 79 files, 758 passed, 2 known timing-budget failures listed in suites-expected.json, exit 0.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Shape keys moved out of Shape into heap-owned, append-only key tables shared along a transition chain (Heap::key_tables; a Shape is table + len + transitions + parent). Adding a property to the shape at a table's tip appends one entry, amortised O(1) in time and memory; a second transition off the same shape forks the table, copying only that prefix; the hash index lives on the table from the 9th key on, and readers go through Heap::shape_lookup / shape_key / shape_keys. The collector marks each table once instead of every shape's full key list. Tests in crates/agnijs-js-vm/tests/shapes.rs (60-add chain = 60 shapes and one table, forks keep both chains exact, 300-property objects read back and delete). engine-startup 0.97 -> 0.63 ms (1.54x; QuickJS-NG 0.17 ms); every other micro-benchmark within 2% of the parent commit's binary (a first branch binary read +5-12% on map-set and method-calls, traced to code layout by rebuilding and alternating); ui-bundle-startup unchanged at 145.8 ms. cargo test -p agnijs-js-vm green; test262 42225/42225, precompile 42225/42225, intl402 981/981; Gable suites 758 passed with only the two known timing failures.
<!-- SECTION:FINAL_SUMMARY:END -->
