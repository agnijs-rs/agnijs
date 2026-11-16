---
id: TASK-103.8.4
title: 'Temporal: PlainDate and calendars'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-16T01:48:00Z'
updated_date: '2026-11-16T03:51:00Z'
labels:
  - temporal
dependencies: []
parent_task_id: TASK-103.8
ordinal: 333000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Calendar dates carry an ISO or ICU4X calendar; calendar resolution and field merging are shared by the other date types.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Temporal.PlainDate is complete with the ISO calendar and every calendar ECMA-402 lists
- [x] #2 Pass counts recorded
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Seam: Calendar (from an identifier, from ToTemporalCalendarIdentifier strings, identifier, has_eras), DateFields (year, month, monthCode, day, era, eraYear), Overflow, ShowCalendar, and PlainDate (new, from_fields, parse, the calendar getters, with, with_calendar, add, subtract, until, since, compare, equals, to_string). RelativeTo from a PlainDate.
2. VM builtins/temporal/calendar.rs: GetTemporalCalendarIdentifierWithISODefault, ToTemporalCalendarIdentifier, PrepareCalendarFields for date fields in property-name order with the specified coercions (ToPositiveIntegerWithTruncation, ToMonthCode, ToPrimitiveAndRequireString), partial reads and IsPartialTemporalObject. Overflow and calendarName options.
3. VM builtins/temporal/plain_date.rs: constructor, from, compare, the getters, with, withCalendar, add, subtract, until, since, equals, toString, toJSON, toLocaleString (toString until 103.8.9), valueOf. Conversions needing later types (toPlainDateTime, toPlainYearMonth, toPlainMonthDay, toZonedDateTime) come with those types.
4. Duration relativeTo accepts a Temporal.PlainDate; property bags come with ZonedDateTime (103.8.8), since a bag may carry a time zone.
5. Tests first; measure; baseline, README, fuzz target.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented as planned. Seam: calendar.rs (Calendar, MonthCode, DateFields) and plain_date.rs; Overflow and ShowCalendar options. VM: builtins/temporal/calendar.rs (ToTemporalCalendarIdentifier, GetTemporalCalendarIdentifierWithISODefault, PrepareCalendarFields for date fields in alphabetical order with era/eraYear only for calendars with eras, IsPartialTemporalObject) and plain_date.rs. Numbers convert saturating (year/eraYear to i32, month/day to u8) so out-of-range values stay RangeErrors at resolution and the missing-field TypeError keeps its place after the overflow option is read. Duration relativeTo accepts a Temporal.PlainDate.

test262 Temporal: 935 -> 1576 of 5168. Temporal/PlainDate (built-ins + intl402): 594 of 762. Every remaining PlainDate failure needs a type that does not exist yet (PlainDateTime, PlainYearMonth, PlainMonthDay, ZonedDateTime; noted on 103.8.6-8) or Intl formatting (toLocaleString, 103.8.9). Fuzz: temporal target extended with PlainDate bags, calendars, month codes and overflow; 114,511 inputs in 120 s clean.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Temporal.PlainDate with the ISO calendar and every ICU4X calendar Temporal supports: constructor, from (strings, property bags with calendar-specific era/eraYear/monthCode fields, overflow), compare, all calendar getters, with, withCalendar, add, subtract, until, since, equals, toString (calendarName), toJSON, toLocaleString (= toString until 103.8.9), valueOf. Shared calendar and property-bag readers in builtins/temporal/calendar.rs serve the later date types. Duration relativeTo accepts a PlainDate. test262 Temporal 935 -> 1576 of 5168; PlainDate 594 of 762, the rest blocked only on later types (conversion methods noted on 103.8.6-8) or Intl formatting. Tests: seam 15, VM temporal 16, all touched crates green; clippy clean on touched files; fuzz 114k inputs clean.
<!-- SECTION:FINAL_SUMMARY:END -->
