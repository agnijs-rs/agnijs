---
id: TASK-110.4
title: Close the interpreter gap to QuickJS-NG on the Gable benches
status: To Do
assignee: []
created_date: '2026-11-16T14:29:00Z'
labels:
  - performance
dependencies: []
parent_task_id: TASK-110
ordinal: 344000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The interpreter takes 1.46 times QuickJS-NG on the Gable benches, an interpreter with no JIT at all (ADR 0028, direction 4). This is the umbrella for what profiles show after the three tasks above.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Gap measured again after the earlier subtasks and reduced, with the remaining hot spots listed
- [ ] #2 Each change has before/after numbers and no regression elsewhere
<!-- AC:END -->
