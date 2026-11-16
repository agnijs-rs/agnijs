---
id: TASK-110.3
title: 'Denser bytecode: fused typeof comparison and profile-driven superinstructions'
status: To Do
assignee: []
created_date: '2026-11-16T14:29:00Z'
labels:
  - performance
dependencies: []
parent_task_id: TASK-110
ordinal: 343000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Dispatch is 21 to 48% of Gable interpreter time (ADR 0028, direction 3).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 typeof x === "..." compiles to one fused instruction, shared with the JIT where it is a known call-out
- [ ] #2 Further fusions are chosen from fresh profiles and each shows a measured gain; bytecode format tag changes and snapshots/precompile are regenerated
- [ ] #3 test262 0 regressions in all modes including --precompile and --snapshot
<!-- AC:END -->
