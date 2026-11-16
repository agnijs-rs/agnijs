---
id: TASK-103.8.2
title: 'Temporal: shared infrastructure and Temporal.Duration'
status: To Do
assignee:
  - '@claude'
created_date: '2026-11-16T01:48:00Z'
labels:
  - temporal
dependencies: []
parent_task_id: TASK-103.8
ordinal: 331000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Every Temporal type reads options in spec order, converts property bags and maps library errors to RangeError or TypeError; Duration is the first type and the one the others return from until/since.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Temporal.Duration and its prototype are complete per the spec (constructor, from, compare, getters, with, negated, abs, add, subtract, round, total, toString, toJSON, toLocaleString, valueOf)
- [ ] #2 Temporal objects are traced, refused by snapshots with a reason, and charged to the memory limit
- [ ] #3 test262 Temporal/Duration pass count recorded in the temporal baseline
<!-- AC:END -->
