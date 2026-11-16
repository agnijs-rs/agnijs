---
id: TASK-110.2
title: 'Cheaper call path: call/apply, getters and setters, new, frame set-up'
status: To Do
assignee: []
created_date: '2026-11-16T14:29:00Z'
labels:
  - performance
dependencies: []
parent_task_id: TASK-110
ordinal: 342000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Calls are 8 to 29% of Gable interpreter time, and call/apply, getters/setters and new are also the calls compiled code cannot make itself (ADR 0028, direction 2).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Function.prototype.call and apply, getters and setters, and new run as frames pushed by the dispatch loop without a native frame between
- [ ] #2 Frame set-up cost is reduced; method-calls and closures micro-benchmarks and Gable benches improve measurably
- [ ] #3 Stack-depth limits, interrupts, the debugger and stack traces behave unchanged; test262 0 regressions in all modes
<!-- AC:END -->
