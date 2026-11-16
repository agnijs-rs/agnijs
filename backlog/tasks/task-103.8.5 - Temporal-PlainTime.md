---
id: TASK-103.8.5
title: 'Temporal: PlainTime'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-16T01:48:00Z'
updated_date: '2026-11-16T04:29:00Z'
labels:
  - temporal
dependencies: []
parent_task_id: TASK-103.8
ordinal: 334000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Wall-clock time with no date is needed by PlainDateTime and ZonedDateTime conversions.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Temporal.PlainTime is complete
- [x] #2 Pass counts recorded
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Seam plain_time.rs: TimeFields (hour..nanosecond as i64), PlainTime (new with reject, from_fields and with under overflow: constrain clamps, reject range-checks, parse, add, subtract, until, since, round, compare, getters, to_string_with). 2. VM plain_time.rs: constructor (ToIntegerWithTruncation each, RangeError outside the time), ToTemporalTime (PlainTime, bags via ToTemporalTimeRecord in alphabetical order, strings; options read after the item), from, compare, the six getters, with (IsPartialTemporalObject, partial record), add, subtract, until/since (time-unit GetDifferenceSettings shared with Instant), round (shared reader with Instant), equals, toString (fractionalSecondDigits, roundingMode, smallestUnit; hour rejected), toJSON, toLocaleString (toString until 103.8.9), valueOf. Conversions from PlainDateTime and ZonedDateTime come with those types. 3. Tests first; measure; baseline, README, fuzz target.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented as planned. Seam plain_time.rs (PlainTime, TimeFields; RegulateTime clamps or rejects on i64 before the library's u8/u16 fields). VM plain_time.rs; Instant's time-unit GetDifferenceSettings and round() reader are shared (instant.rs: time_difference_settings, read_round_to, require_time_unit).

test262 Temporal: 1576 -> 2052 of 5168; Temporal/PlainTime (built-ins + intl402) 480 of 595. The remaining PlainTime failures need PlainDateTime or ZonedDateTime (noted on 103.8.6 and 103.8.8) or Intl formatting (103.8.9). Fuzz: temporal target extended with PlainTime; 107,604 inputs in 120 s clean.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Temporal.PlainTime: constructor, from (strings, property bags with overflow), compare, the six field getters, with, add, subtract, until, since, round, equals, toString (fractionalSecondDigits, roundingMode, smallestUnit), toJSON, toLocaleString (= toString until 103.8.9), valueOf. Shares Instant's time-unit option readers. test262 Temporal 1576 -> 2052 of 5168; PlainTime 480 of 595, the rest blocked only on PlainDateTime/ZonedDateTime (noted on 103.8.6 and 103.8.8) or Intl formatting. Tests: seam 17, VM temporal 19, all touched crates green; clippy clean on touched files; fuzz 107k inputs clean.
<!-- SECTION:FINAL_SUMMARY:END -->
