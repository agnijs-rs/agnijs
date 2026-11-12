---
id: TASK-103.33
title: Debugger pause requests must reach long-running native calls
status: To Do
assignee: []
created_date: '2026-11-12T20:07:00Z'
labels:
  - enhancement
dependencies: []
parent_task_id: TASK-103
priority: low
ordinal: 297000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Left over from TASK-103.28. Interrupts now reach regex matches and native built-in loops, but a CDP/debugger pause request is only taken at the next statement, so it lands after a long native call returns. Make pause polling share the native-loop poll points so Debugger.pause stops a long native call partway, or document why it cannot.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Red test: a pause request during a long native call (join over 3,000,000 elements, pathological regex) is taken within 2 s
- [ ] #2 No benchmark regression beyond 1%; test262 0 regressions in all modes
<!-- AC:END -->
