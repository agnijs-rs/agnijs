---
id: TASK-110.1
title: Megamorphic property-access cache shared by the interpreter and compiled code
status: To Do
assignee: []
created_date: '2026-11-16T14:28:00Z'
labels:
  - performance
dependencies: []
parent_task_id: TASK-110
ordinal: 341000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Out-of-line property access is 15 to 44% of Gable interpreter time, and megamorphic GetNamed is 78% of what compiled signal-churn leaves machine code for (ADR 0028, direction 1).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A cache for sites with more than four shapes, keyed by shape and name, serves both the interpreter and JIT-compiled code
- [ ] #2 Gable signal-churn and rxjs benches improve measurably; micro-benchmark geomean stays within 1% elsewhere
- [ ] #3 test262 0 regressions in all modes, GC-stress slice and JIT fuzz clean, memory cost of the cache measured and bounded
<!-- AC:END -->
