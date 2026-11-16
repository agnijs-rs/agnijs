---
id: TASK-103.8.2
title: 'Temporal: shared infrastructure and Temporal.Duration'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-16T01:48:00Z'
updated_date: '2026-11-16T02:50:00Z'
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
- [x] #1 Temporal.Duration and its prototype are complete per the spec (constructor, from, compare, getters, with, negated, abs, add, subtract, round, total, toString, toJSON, toLocaleString, valueOf)
- [x] #2 Temporal objects are traced, refused by snapshots with a reason, and charged to the memory limit
- [x] #3 test262 Temporal/Duration pass count recorded in the temporal baseline
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Seam: own Unit, RoundingMode, RoundingOptions, Precision, ToStringOptions; Duration (opaque newtype over temporal_rs::Duration) with DurationFields (i64, i128 for micro/nanoseconds), parse, new, sign, negated, abs, add, subtract, compare, round, total, to_string; RelativeTo from a string. Every library call goes through guard(), which turns a panic into an Internal error.
2. VM: Class::Temporal(Box<TemporalData>) behind the feature; GC trace has nothing to visit (no heap references); snapshot refuses it with a reason; payload_bytes charges the boxed data to the memory limit.
3. VM builtins/temporal/options.rs: GetOptionsObject, GetOption for strings, unit-valued options, rounding mode, rounding increment, fractional second digits, ToIntegerWithTruncation, ToIntegerIfIntegral, each one Get in spec order.
4. VM builtins/temporal/duration.rs: constructor, from, compare, the field getters, sign, blank, with, negated, abs, add, subtract, round, total, toString, toJSON, toLocaleString (toString until Intl.DurationFormat), valueOf. relativeTo accepts strings now; Temporal objects and property bags arrive with PlainDate (103.8.4) and ZonedDateTime (103.8.8).
5. Tests first in tests/temporal.rs (values, error types, option read order); then measure --only-temporal and update the baseline and the README row.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Verified: seam 8 tests, VM tests/temporal.rs 7 tests (values, error types, option read order, snapshot refusal, memory limit), full agnijs-js-vm suite with and without the feature (only stack_budget fails, identically on the base commit ad3e131: release builds hit the regex nesting limit first, unrelated). temporal fuzz target: 20,007 inputs clean. test262 Temporal 48 -> 466/5168; Temporal/Duration 409/562. The remaining Duration failures need PlainDate/ZonedDateTime objects and property bags as relativeTo (103.8.4, 103.8.8) or Intl.DurationFormat for toLocaleString (outside the cut). Results are rounded through f64 per field as CreateTemporalDuration requires.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Temporal.Duration (constructor, from, compare, getters, with, negated, abs, add, subtract, round, total, toString, toJSON, toLocaleString, valueOf) and the shared option readers, behind the temporal feature. The seam wraps temporal_rs with its own types and runs every call under catch_unwind. Class::Temporal is traced (no references), refused by snapshots with a reason and charged to the memory limit. New temporal fuzz target. test262 Temporal 466/5168 (Duration 409/562). Follow-ups: relativeTo objects with 103.8.4/103.8.8.
<!-- SECTION:FINAL_SUMMARY:END -->
