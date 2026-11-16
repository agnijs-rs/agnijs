---
id: TASK-103.8.6
title: 'Temporal: PlainDateTime and the remaining Now methods'
status: To Do
assignee:
  - '@claude'
created_date: '2026-11-16T01:48:00Z'
updated_date: '2026-11-16T04:05:00Z'
labels:
  - temporal
dependencies: []
parent_task_id: TASK-103.8
ordinal: 335000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Date plus wall-clock time, and Temporal.Now.plainDateTimeISO, plainDateISO and plainTimeISO.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Temporal.PlainDateTime and the plain Now methods are complete
- [ ] #2 Pass counts recorded
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Includes Temporal.PlainDate.prototype.toPlainDateTime and ToTemporalDate accepting a PlainDateTime (left out of 103.8.4 because the type did not exist yet).

Includes ToTemporalTime accepting a PlainDateTime and PlainDateTime.prototype.toPlainTime (left out of 103.8.5).
<!-- SECTION:NOTES:END -->
