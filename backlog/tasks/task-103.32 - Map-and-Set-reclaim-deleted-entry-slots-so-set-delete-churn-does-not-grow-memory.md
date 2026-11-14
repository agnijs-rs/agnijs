---
id: TASK-103.32
title: >-
  Map and Set: reclaim deleted-entry slots so set/delete churn does not grow
  memory
status: Done
assignee:
  - '@claude'
created_date: '2026-11-12T16:28:00Z'
updated_date: '2026-11-14T09:41:00Z'
labels:
  - bug
dependencies: []
parent_task_id: TASK-103
priority: medium
ordinal: 296000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found while doing TASK-103.25. A deleted Map or Set entry leaves an empty slot that is kept until the collection is freed, so a loop of m.set(k,v); m.delete(k) grows memory without bound (about 32 bytes per round, not yet measured) and, once collection slots are counted in the memory limit, would hit the limit. Reclaiming slots needs a change to how Map/Set iterators track their position (e.g. compaction with iterator index fix-up). Must keep iteration order and live-iterator semantics per spec.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Red test: set/delete churn on a Map and a Set keeps memory_usage bounded
- [x] #2 Live iterators stay correct across deletes and compaction (test262 collections tests and new tests)
- [x] #3 Benchmark geomean within 1%, test262 0 regressions in all modes
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red tests: memory_usage counts every entry slot (tombstones included), so set/delete churn on a Map and a Set grows it; a peak-allocator test in agnijs-js-bench shows the process allocation growing; live-iterator tests (deletes ahead/behind, clear, re-add, forEach and Set methods mutating mid-loop) in tests/collections.rs.
2. Each slot carries an insertion sequence number. Iterators, forEach and the Set-method loops hold a Cursor (index hint + sequence) and resolve it with a check of the hint or a binary search, so positions survive compaction.
3. Strong collections compact like weak ones once tombstones outnumber live entries (more than 16); clear drops the slots.
4. Gates: vm tests, test262 all modes, GC stress, JIT slices, benchmarks (map-set) within 1%.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented in 1e6d1e2. Measured before the fix: 2,000,000 set/delete rounds on a Map and a Set allocated 128 MiB (32 B per round per collection, the peak-allocator test). Slots carry an insertion sequence number; MapIterState, forEach and the Set-method loops hold a Cursor (index hint + seq) resolved by a two-comparison check or a binary search after a compaction. compact_if_sparse now runs for all collections (dead > 16 and dead > live), clear drops all slots. heap_bytes counts every slot (tombstones included), so memory_usage shows the churn; MAP_ENTRY_BYTES is 81 (slot 40 with the seq). Gates so far: vm tests, clippy on changed lines, test262 0 regressions in default, --precompile, --snapshot, --only-intl402, QuickJS-NG baseline, GC stress 16 slice (+ Map/Set dirs), JIT force/on slice (+ Map/Set dirs).

Gates (worktree, after 103.32/33/31 stacked): test262 default 43192/43192, --precompile, --snapshot, --only-intl402 (981), QuickJS-NG baseline, GC stress (6568, includes built-ins/Map and Set), JIT force/on slices (4224, include Map and Set): 0 regressions everywhere. Benchmarks vs 4d4de18, JIT off, release, interleaved: 4 rounds over all 26 workloads, pooled geomean 0.9997 (new/old). map-set alone, 8 more rounds at 10 iterations: 1.031 (every round 1.01 to 1.10): the slot grew from 32 to 40 bytes for its sequence number. A u32 would not shrink it (Value's alignment), and dropping the sequence for a compaction log would need to know which iterators are alive, so the log could grow without bound again; accepted. calls-recursive, method-calls, closures and startup were noise in the rerun (0.99 to 1.01).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Map and Set now compact their deleted-entry slots once empty slots outnumber live entries and pass 16, so set/delete churn no longer grows the process (128 MiB for two million rounds before, under 8 MiB now) and the memory limit counts every slot (81 bytes an entry). Each slot carries an insertion sequence number; iterators, forEach and the Set methods hold a Cursor (index hint plus sequence number) and find their place again after a compaction or clear. Tests: tests/collections.rs and a peak-allocator test in agnijs-js-bench. test262 0 regressions in all modes; benchmark geomean 0.9997, with map-set about 3% slower from the larger slot (see notes).
<!-- SECTION:FINAL_SUMMARY:END -->
