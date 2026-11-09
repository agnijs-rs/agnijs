---
id: TASK-100.23
title: >-
  Engine: count element, slot and dictionary growth towards collection and the
  memory limit
status: In Progress
assignee:
  - '@claude-fable'
created_date: '2026-11-09T03:18:00Z'
updated_date: '2026-11-09T06:25:00Z'
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
- [x] #1 Growth of object elements, property slots, dictionaries and string buffers is added to the allocation counter used for GC triggering and for the memory limit
- [x] #2 A test shows a loop that only grows arrays triggers collections and hits a configured memory limit with an out-of-memory error instead of growing past it
- [ ] #3 array-methods peak RSS falls to within 2x of QuickJS-NG without a time regression above 5%
- [x] #4 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [x] #5 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Reproduce (red): tests in crates/agnijs-js-vm/tests/gc.rs showing a loop that only pushes numbers onto fresh arrays runs zero collections and sails past a 4 MiB memory limit (ticket reproduction, scaled to stay bounded in a debug test build).
2. Account for growth where it happens (crates/agnijs-js-vm/src/heap.rs and object.rs): alloc_object counts the object's initial slots/elements/dict (new_array with items); array_push_dense, add_own_property (dense append and resize, sparse insert, slot push, dict insert), make_dictionary and make_sparse add the new storage bytes; flatten counts the flat string buffer. All through one Heap::note_growth helper. Threshold stays max(2 x live, 8 MiB) (ADR 0009); the run-loop memory-limit check is unchanged and benefits automatically.
3. Green: the new tests, then cargo test -p agnijs-js-vm, then the workspace tests.
4. Measure before/after with release bench.exe built from origin/main and from this branch: the ticket reproduction (collections, peak RSS), array-methods, the full micro set via bench compare; Gable list-diff via the harness with the engine path-patched.
5. test262: gable, gable --precompile, intl402, once each at the end.
6. Record numbers in task notes, check ACs, final summary, push to main.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Root cause confirmed on origin/main a80ff9e (release, i9-9900KF): the ticket reproduction (20 000 arrays x 1 000 pushes, classic script) ran 0 collections, heap_bytes 323.6 MB, peak RSS 323.5 MiB; array-methods x6 (1 warmup + 5) ran 0 collections, heap_bytes 81.7 MB over 21 138 objects, peak RSS 91.4 MiB. Only the 160-byte object header was counted per array.

After counting growth (same machine, same session): reproduction 38 collections, heap_bytes 5.2 MB after the loop, peak RSS 16.8 MiB (19x less), time 7715 ms vs 7712 ms before; array-methods x6: 9 collections, heap_bytes 6.6 MB, peak RSS 16.8 MiB (was 91.4), time 3763 ms vs 3712 ms (+1.4%, within run-to-run noise).

Micro-benchmarks (bench run, both engines, 1 warmup + 5 iterations, before = origin/main a80ff9e bench.exe, after = this branch): bench compare shows every agnijs time within +-4% (prop-write-alloc 1.04, loop-arith 1.03 on a loop that does not allocate, array-methods 1.00 at 641 ms) and three improvements: promise-chain 62.9 -> 44.4 ms (0.71), closures 0.94, async-await 0.94. Peak RSS (MiB, agnijs before -> after, QuickJS-NG after): array-methods 105.2 -> 16.2 (5.2); prop-write-alloc 35.3 -> 19.7; string-concat 33.4 -> 18.9; map-set 45.6 -> 31.2; closures 38.2 -> 20.2; regex 38.9 -> 19.3; json-parse 39.6 -> 22.0; promise-chain 161.3 -> 146.6 (22.1); engine-startup 21.7 -> 7.1 (5.2). The before run carried the ~14 MiB first-run-after-rebuild offset the baselines README describes on all but its first pair; the clean before figure for array-methods is 91.4 MiB (scratch run, matches the checked-in baseline's 91.2). AC 3 caveat: 16.2 MiB is 3.1x QuickJS-NG's 5.2 MiB; the engine's own floor is 7.1 MiB (engine-startup) and the ADR 0009 pacing floor of 8 MiB, which the ticket says to keep, bounds the rest. 2x of QuickJS (10.4 MiB) is below floor + pacing; the ticket's 2x was written against the findings doc's inflated pair (105.6 vs 20.3).

Gable benchmarks (run-bench.mjs, 1 warmup + 5 iterations, gable-harness built twice with the engine crates path-patched to origin/main a80ff9e and to this branch; Gable's lockfile restored): no time regression. agnijs ms before -> after: rxjs-operators 422 -> 416, rxjs-higher-order 564 -> 592 (re-run alone, 2 warmup + 7 iterations, alternating binaries: 573/570 before vs 574/566 after, so noise), rxjs-subject-fanout 315 -> 310, signal-churn 964 -> 966, signal-create 192 -> 192, template-mount 129 -> 129, template-update 77 -> 79 (re-run 75/76 vs 78/78, +2%), list-diff 1296 -> 1323 (+2%), ui-bundle-startup 147 -> 146. Peak RSS unchanged (list-diff 237.5 -> 237.3 MiB, rxjs-higher-order 124 -> 124.1, QuickJS-NG 40.7 and 6.3): these run as modules inside native frames, where the collector is deferred (finding 1), so counted growth cannot trigger a collection there until TASK-100.16 lands; the ticket already scopes list-diff peak RSS as 'after TASK-100.16'.

Verification: cargo test -p agnijs-js-vm green (78 tests, including the two new gc.rs tests and the stress-mode programs). test262 on this branch, release, one run each: --engine agnijs 42225/42225, --engine agnijs --precompile 42225/42225, --only-intl402 981/981, 0 regressions and 0 newly passing in all three. Gable suites under the agnijs engine (run-suites.mjs with the path-patched harness): 79 files, 758 passed, 2 known timing-budget failures listed in suites-expected.json, exit 0.
<!-- SECTION:NOTES:END -->

## Comments

<!-- COMMENTS:BEGIN -->
author: @claude-fable
created: 2026-11-09T06:25:00Z

---

AC 3 left unchecked for the maintainer: array-methods peak RSS is 16.2 MiB against QuickJS-NG's 5.2 MiB (3.1x; before: 91.4 MiB, 17.5x). The 2x target (10.4 MiB) sits below the engine's own 7.1 MiB startup floor plus the 8 MiB pacing floor from ADR 0009 that this ticket says to keep; the ticket's 2x was written against the findings doc pair (105.6 vs 20.3 MiB), where both engines carried the ~15 MiB first-run offset. Either accept the criterion relative to the floor, or open a follow-up on the pacing floor (an ADR 0009 change, with a time cost on the allocation-heavy workloads per finding 7).

---
<!-- COMMENTS:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Element, slot, dictionary and flattened-string growth now counts towards allocated_since_gc, the counter that drives both collection pacing and the memory-limit check (crates/agnijs-js-vm/src/heap.rs: Heap::note_growth, alloc_object counts the initial extra_bytes, flatten counts the flat buffer; object.rs: array_push_dense, add_own_property, make_dictionary and make_sparse count the storage they add, at the same per-entry sizes approx_live_bytes uses). The threshold stays max(2 x live, 8 MiB) per ADR 0009. Two tests in tests/gc.rs reproduce the ticket: a loop that only pushes numbers onto fresh arrays now collects (was 0 collections) and hits a 4 MiB memory limit with Error::OutOfMemory (previously ran to completion). Results: reproduction 0 -> 38 collections, peak RSS 323.5 -> 16.8 MiB; array-methods x6 0 -> 9 collections, peak RSS 91.4 -> 16.2 MiB (QuickJS-NG 5.2), time unchanged (641 ms both); every other micro-benchmark within +-4%, promise-chain 29% faster; Gable set unchanged in time and in peak RSS (module code runs under deferred collection until TASK-100.16). test262 42225/42225, precompile 42225/42225, intl402 981/981; Gable suites pass. Open: AC 3's literal 2x-of-QuickJS peak is not reachable above the engine floor plus the 8 MiB pacing floor (see comment); the maintainer decides whether to accept it or follow up on the pacing floor.
<!-- SECTION:FINAL_SUMMARY:END -->
