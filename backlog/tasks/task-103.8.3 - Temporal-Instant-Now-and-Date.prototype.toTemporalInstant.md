---
id: TASK-103.8.3
title: 'Temporal: Instant, Now and Date.prototype.toTemporalInstant'
status: To Do
assignee:
  - '@claude'
created_date: '2026-11-16T01:48:00Z'
labels:
  - temporal
dependencies: []
parent_task_id: TASK-103.8
ordinal: 332000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Exact time is the base of ZonedDateTime and the only Temporal type Date converts to; Now reads the host clock and zone.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Temporal.Instant, Temporal.Now.instant and timeZoneId, and Date.prototype.toTemporalInstant are complete
- [ ] #2 Now takes its time and zone only from Host hooks
- [ ] #3 Pass counts recorded
<!-- AC:END -->
