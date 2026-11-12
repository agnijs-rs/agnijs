---
id: TASK-100.35
title: Close the GC memory gap to QuickJS-NG
status: To Do
assignee: []
created_date: '2026-11-10T07:02:00Z'
updated_date: '2026-11-12T03:43:00Z'
labels:
  - needs-triage
dependencies: []
parent_task_id: TASK-100
priority: medium
ordinal: 284000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
TASK-100.16 let the collector run under native frames and cut peak memory sharply, but Gable benches still peak well above QuickJS-NG (rxjs-higher-order 35 MiB vs 6.7; list-diff 165 vs 42; reproduction 17.6 vs 5.4). The remaining gap comes from the 8 MiB default collection threshold, the process baseline, the 2x live growth factor, and garbage born before a host call that a depth-1 loop retains until the call returns. Model assignment: Opus. Context: TASK-100.16 notes and final summary.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 The default collection threshold (8 MiB), the growth factor and the depth-1 loop retention of pre-host-call garbage are each measured and tuned, with before and after numbers recorded in the task notes
- [ ] #2 Peak RSS of the Gable benches in run-bench.mjs (at least rxjs-higher-order and list-diff) is within 2.5x of QuickJS-NG
- [x] #3 No Gable bench or micro-benchmark regresses by more than 5% in time
- [x] #4 GC stress mode (AGNIJS_GC_STRESS) stays green on the unit subset and the test262 slice, with no wrong results
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Measure: micro-benchmarks (bench run, both engines, peak RSS) and the Gable benches (gable-harness rebuilt against this worktree through a cargo patch) plus the gc-top.mjs reproduction, with AGNIJS_GC_TRACE for collection counts and live sizes. 2. Host-call retention: a host call (engine::call/construct, native_depth 0, no frames) holds only handles, so garbage born before it need not be retained; collect at the host-call safe point when a sizeable fraction of the threshold is already allocated, so the next loop does not retain it. 3. Default threshold: lower from 8 MiB (try 4 and 2 MiB) and measure collection cost on the allocation-heavy micro-benchmarks; keep the hot loop within 1% geomean. 4. Growth factor: try 1.5x live (QuickJS-NG's) against 2x on list-diff. 5. Tests first for each seam (gc tests: threshold default, host-call collection, growth factor). 6. Commit and push after each verified step; run test262 default and --precompile, intl402, GC stress slice.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Baseline (main 93ab51e, gable-harness rebuilt against the worktree, run-bench.mjs medians, peak RSS MiB agnijs / quickjs): rxjs-higher-order 289.7 ms 34.9 / 6.7; rxjs-subject-fanout 123.8 ms 19.2 / 6.6; signal-create 90.7 ms 29.7 / 8.0; template-mount 80.9 ms 43.0 / 17.8; list-diff 857.3 ms 164.7 / 42.0. Reproduction gc-top.mjs 18.2 / 5.4 MiB. GC trace: rxjs-higher-order lives at 450 KB with the 8 MiB floor; list-diff lives at 20 MiB with a 40.8 MiB threshold and retained 17 to 75 MB of pre-job garbage at each in-series collection (alternating 40.8 and 81.6 MiB put-off collections).

Step 1 (commit b437333): free safe points (host-call entry, job boundary) collect once half the threshold base is allocated (Heap::wants_free_gc). Same build, agnijs only: rxjs-higher-order 261.0 ms 21.3 MiB; rxjs-subject-fanout 123.2 ms 19.2; signal-create 89.5 ms 29.5; template-mount 72.5 ms 42.7; list-diff 851.2 ms 130.7. With AGNIJS_GC_THRESHOLD=4194304: 254.9 ms 15.0; 127.9 ms 19.3; 91.2 ms 22.0; 76.5 ms 42.8; 860.8 ms 129.9. With 2097152: 260.2 ms 13.3; 125.9 ms 13.8; 91.5 ms 18.7; 75.5 ms 43.0; 857.8 ms 131.5. Times within the run-to-run band at every floor.

Threshold floor: micro-benchmarks (bench run, agnijs, 5 iterations) at the 8 MiB floor against 2 MiB, both with step 1: geomean 2 MiB / 8 MiB = 0.988 (regex-long-input excluded), no workload slower by more than the run-to-run band (calls-recursive 55.96 to 58.09 ms, +3.8%, allocates nothing that collections could affect; everything else within 2.5%, several faster). Peak RSS at 8 / 2 MiB: prop-write-alloc 16.3 / 9.6, string-concat 19.8 / 9.5, string-methods 17.8 / 9.5, array-methods 15.4 / 8.9, map-set 37.1 / 14.8, closures 20.3 / 10.0, regex 19.7 / 10.2, regex-tokenize 20.1 / 10.2, json-stringify 16.1 / 10.0, json-parse 21.7 / 15.0, async-await 15.8 / 8.8; the loop, call and promise-chain workloads unchanged (6.4 to 6.9; promise-chain 39.5 either way, its live set is 23 MB). The 2 MiB floor is the default from step 2. Reproduction gc-top.mjs with it: 9.0 MiB (was 18.2; QuickJS-NG 5.3).

Growth factor (AGNIJS_GC_GROWTH, 2 MiB floor): 200 vs 150 percent: list-diff 865.5 ms 132.6 MiB vs 891.4 ms 124.5 MiB (+3% time for -6% memory); template-mount 73.4 ms 43.0 vs 71.9 ms 41.0; signal-churn 471.9 ms 15.4 vs 468.8 ms 12.5. Kept at 2x: list-diff's gap is not the factor. Its trace at the 2 MiB floor: live 20.4 MB, each in-series collection at the 40.8 MB threshold still retains about 17 MB of garbage from the jobs before the flush (bounded by half a threshold now), and the RSS is about 1.6x the counted bytes (slot-heavy objects, strings, shapes).

Validation (commits 7cb93e6 and 90672e0 on main, rebased on f241b36): test262 default 42231/42231, --precompile 42231/42231, intl402 981/981, 0 regressions; GC stress slice at AGNIJS_GC_STRESS=16 (class, assignment, for-of: 5949 tests) 5949 passed, 0 failed, 0 wrong results; AGNIJS_GC_STRESS=1 unit tests of agnijs-js-vm and agnijs-js-compiler 257 passed; agnijs-capi under stress (basics, hostile, fault_injection) 44 passed; jit feature build with jit_differential, jit_inline_cache, gc_native_frames and gc tests 75 passed; VM crate without stress 231 passed. Final Gable benches (run-bench.mjs, same harness build, agnijs ms / peak MiB vs quickjs ms / peak MiB): rxjs-operators 185.4 / 8.4 vs 100.6 / 6.6; rxjs-higher-order 269.0 / 13.3 vs 184.0 / 6.7 (1.99x, was 5.2x); rxjs-subject-fanout 121.2 / 13.8 vs 79.2 / 6.6 (2.1x, was 2.9x); signal-churn 467.1 / 15.5 vs 238.2 / 9.1; signal-create 86.2 / 18.8 vs 65.7 / 7.9 (2.4x, was 3.7x); template-mount 72.8 / 42.5 vs 52.9 / 17.9 (2.4x); template-update 40.3 / 14.3 vs 28.7 / 10.1; list-diff 838.2 / 132 vs 774.6 / 42.4 (3.1x, was 3.9x); ui-bundle-startup 95.5 / 41.4 vs 73.1 / 19.6. Geomean agnijs/quickjs 1.45 (was 1.35 on the five-bench baseline subset; on the same five: 1.39 vs 1.35, each within the band: rxjs-higher-order 289.7 to 269.0 ms, template-mount 80.9 to 72.8, list-diff 857.3 to 838.2, signal-create 90.7 to 86.2, rxjs-subject-fanout 123.8 to 121.2). No Gable bench or micro-benchmark is slower by more than 5%.

Independent re-measurement 2026-11-11 (main 8972adb, same i9-9900KF, 3 runs of run-bench.mjs at 5 iterations / 1 warmup, gable-harness from Gable 7cd58eb with path dependencies on this commit; full tables in docs/js-engine-performance-findings.md, section Re-measurement 2026-11-11). Peak RSS agnijs / quickjs MiB: rxjs-higher-order 13.1 / 6.5 (2.02x, within the AC 2 bound); list-diff 123.8 / 40.9 (3.03x, still outside it); template-mount 41.4 / 17.8 (2.33x); ui-bundle-startup 41.5 / 19.3 (2.15x); rxjs-subject-fanout 13.9 / 6.6 and signal-create 16.7 / 7.9 (2.11x). Gable RSS geomean 1.92x (3.98x at 3ade2ef); time geomean 1.46x (2.86x at 3ade2ef), runs 1.455 to 1.467. These match this task's final numbers within the run-to-run band, so AC 2 stands as reported: met for rxjs-higher-order, not for list-diff. Micro set: time geomean 0.92x over the 20 original workloads, micro RSS 1.64x (floor-dominated). Caveat: the first bench run after a build reports micro RSS about 15 MiB high; the figures above discard it.
<!-- SECTION:NOTES:END -->

## Comments

<!-- COMMENTS:BEGIN -->
author: @fable
created: 2026-11-10T23:55:00Z

---

AC 2 holds for rxjs-higher-order (1.99x) but not list-diff (3.1x); left for maintainer judgement as with TASK-100.16, task left In Progress.

---
<!-- COMMENTS:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Two pacing changes close most of the remaining memory gap. (1) Free safe points (a host call's entry, the boundary between jobs) collect once half the threshold base is allocated (Heap::wants_free_gc): garbage left since the last collection was otherwise kept by every nested run loop entered after it, so list-diff carried 17 to 75 MB of pre-flush garbage through each flush. (2) The collection threshold floor is 2 MiB instead of 8 (a small-heap collection marks in 0.2 ms; the micro-benchmark geomean is 0.988 of the 8 MiB build), and AGNIJS_GC_THRESHOLD / AGNIJS_GC_GROWTH override floor and growth factor for experiments; the growth factor stays 2x (1.5x bought list-diff 6% memory for 3% time). Results: rxjs-higher-order 34.9 to 13.3 MiB (QuickJS-NG 6.7, 1.99x), rxjs-subject-fanout 19.2 to 13.8 (6.6), signal-create 29.7 to 18.8 (7.9), list-diff 164.7 to 132 (42.4, 3.1x), reproduction 18.2 to 9.0 (5.3); times within the run-to-run band everywhere (list-diff and rxjs-higher-order faster). test262 42231/42231 default and precompile, intl402 981/981, GC stress slice and unit subsets clean, C API stress clean, jit feature green. Not met: AC 2 for list-diff (3.1x): its live set is 20 MB, so the threshold is 41 MB, each in-series collection still retains up to half a threshold of pre-flush garbage, and RSS is about 1.6x the counted bytes. Follow-ups: let a callback series collect at its own bound (free pre-loop garbage inside the series) or use a lower growth factor for large heaps; both are measured but unimplemented. Docs: ADR 0009 pacing line, findings doc section 1.
<!-- SECTION:FINAL_SUMMARY:END -->
