---
id: TASK-100.35
title: Close the GC memory gap to QuickJS-NG
status: To Do
assignee: []
created_date: '2026-11-10T07:02:00Z'
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
- [ ] #1 The default collection threshold (8 MiB), the growth factor and the depth-1 loop retention of pre-host-call garbage are each measured and tuned, with before and after numbers recorded in the task notes
- [ ] #2 Peak RSS of the Gable benches in run-bench.mjs (at least rxjs-higher-order and list-diff) is within 2.5x of QuickJS-NG
- [ ] #3 No Gable bench or micro-benchmark regresses by more than 5% in time
- [ ] #4 GC stress mode (AGNIJS_GC_STRESS) stays green on the unit subset and the test262 slice, with no wrong results
<!-- AC:END -->
