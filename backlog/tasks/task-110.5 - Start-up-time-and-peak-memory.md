---
id: TASK-110.5
title: Start-up time and peak memory
status: To Do
assignee: []
created_date: '2026-11-16T14:29:00Z'
labels:
  - performance
dependencies: []
parent_task_id: TASK-110
ordinal: 345000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Start-up is 3.8 times QuickJS-NG on the micro-benchmark and peak memory 1.9 times on Gable benches; a UI framework users meet start-up first (ADR 0028, direction 5).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Start-up profile taken and the largest costs removed or deferred, with before/after numbers
- [ ] #2 Peak memory on the Gable benches reduced, with the largest consumers identified
- [ ] #3 No regression in throughput benchmarks beyond the 1% budget
<!-- AC:END -->
