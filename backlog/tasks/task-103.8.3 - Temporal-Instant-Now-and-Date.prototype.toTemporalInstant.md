---
id: TASK-103.8.3
title: 'Temporal: Instant, Now and Date.prototype.toTemporalInstant'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-16T01:48:00Z'
updated_date: '2026-11-16T03:12:00Z'
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
- [x] #1 Temporal.Instant, Temporal.Now.instant and timeZoneId, and Date.prototype.toTemporalInstant are complete
- [x] #2 Now takes its time and zone only from Host hooks
- [x] #3 Pass counts recorded
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Seam: Instant (opaque over temporal_rs::Instant): new(i128 ns), parse, epoch_nanoseconds, epoch_milliseconds, add, subtract, until, since, round, to_string_with(ToStringOptions, Option<&TimeZone>); TimeZone::parse (ParseTemporalTimeZoneString) and identifier. All through guard().
2. VM builtins/temporal/instant.rs: constructor (ToBigInt, range check), from, fromEpochMilliseconds, fromEpochNanoseconds, compare, epochMilliseconds, epochNanoseconds, add, subtract, until, since (GetDifferenceSettings order: largestUnit, roundingIncrement, roundingMode, smallestUnit; date units RangeError), round, equals, toString (fractionalSecondDigits, roundingMode, smallestUnit, timeZone), toJSON, toLocaleString (toString until 103.8.9), valueOf. toZonedDateTimeISO arrives with ZonedDateTime (103.8.8).
3. VM builtins/temporal/now.rs: Temporal.Now with toStringTag, instant() from Host::now_ns and timeZoneId() from Host::default_time_zone (case-normalized, UTC when the host names an unknown zone). Date.prototype.toTemporalInstant (thisTimeValue, NumberToBigInt).
4. Tests first in tests/temporal.rs including a host with a pinned clock and zone; then measure --only-temporal and update baseline and README.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Verified: seam 10 tests; VM tests/temporal.rs 13 tests (instant values, arithmetic, differences, rounding, strings with time zones, error types, option read order, Date conversion, Now under a host with a pinned clock and zone). Full agnijs-js-vm suite with and without the feature: only the pre-existing stack_budget failure. temporal fuzz target extended with Instant and Date entry points: 20,009 inputs clean. test262 Temporal 466 -> 935/5168: Instant 446/484, Date.prototype.toTemporalInstant 8/8, Now 19/69. Remaining Instant failures: toZonedDateTimeISO and ZonedDateTime arguments (103.8.8), toLocaleString formatting (103.8.9). Remaining Now failures are plainDateISO/plainDateTimeISO/plainTimeISO/zonedDateTimeISO (103.8.6, 103.8.8). Now.timeZoneId falls back to UTC when the host names an unknown zone.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Temporal.Instant (constructor, from, fromEpochMilliseconds, fromEpochNanoseconds, compare, epoch getters, add, subtract, until, since, round, equals, toString with timeZone, toJSON, toLocaleString, valueOf), Temporal.Now.instant and timeZoneId from the Host hooks only, and Date.prototype.toTemporalInstant. The seam gains Instant and TimeZone. test262 Temporal 935/5168 (Instant 446/484). toZonedDateTimeISO follows with ZonedDateTime (103.8.8).
<!-- SECTION:FINAL_SUMMARY:END -->
