---
id: TASK-103.8.8
title: 'Temporal: ZonedDateTime and time zones'
status: To Do
assignee:
  - '@claude'
created_date: '2026-11-16T01:48:00Z'
updated_date: '2026-11-16T03:35:00Z'
labels:
  - temporal
dependencies: []
parent_task_id: TASK-103.8
ordinal: 337000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Exact time in an IANA or offset time zone, with disambiguation and offset options; time zone identifiers are canonicalized consistently with Intl.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Temporal.ZonedDateTime and Temporal.Now.zonedDateTimeISO are complete
- [ ] #2 Time zone ids agree between Temporal and Intl.DateTimeFormat
- [ ] #3 Pass counts recorded
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Includes Temporal.PlainDate.prototype.toZonedDateTime, ToTemporalDate accepting a ZonedDateTime, Duration relativeTo property bags, and ZonedDateTime.prototype.toPlainDate (left out of 103.8.4).
<!-- SECTION:NOTES:END -->
