---
id: TASK-103.8
title: Temporal
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-09T02:14:00Z'
updated_date: '2026-11-16T01:47:00Z'
labels:
  - needs-triage
dependencies: []
parent_task_id: TASK-103
ordinal: 243000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Temporal is the feature developers notice missing. Evaluate adopting the temporal_rs crate (with ICU4X time-zone data) behind a seam like regress and gable-js-intl, versus building it, and implement the Temporal built-ins, including the host time-zone and clock hooks and the determinism work. Model: Opus.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 An ADR decides adopt versus build, and the seam keeps the dependency replaceable
- [ ] #2 The Temporal test262 suites pass at agreed baselines with documented exclusions, and the host default time zone and clock hooks are honoured
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Assessment (2026-11-15): the engine has no Temporal at all. test262 at the pin has 4613 tests under test/built-ins/Temporal, 2040 under test/intl402/Temporal and 81 elsewhere tagged Temporal (Date.prototype.toTemporalInstant, Intl.DateTimeFormat with Temporal objects); all are skipped because Temporal and canonical-tz are in post-es2025-features.txt. Date already takes its clock and zone from the host (ADR 0014, Host::now_ms, default_time_zone); Intl gets IANA offsets from jiff with the bundled jiff-tzdb (ADR 0015).
2. Dependency: adopt temporal_rs (Boa project; the Temporal library of V8, Boa and Kiesel; MIT OR Apache-2.0; no host access or tz data without its default features) behind a new seam crate agnijs-js-temporal, with timezone_provider CompiledTzdbProvider over jiff-tzdb, the tz data Intl already links. Options and reasons in ADR 0027 (subtask 1). The seam exposes its own types and errors, never temporal_rs ones.
3. Temporal is a cargo feature of agnijs-js-vm, off by default until the suites are near complete, so default binary size and startup are unchanged while it grows; the test262 runner and the fuzz crate enable it. temporal_rs needs Rust 1.86, so the seam crate declares rust-version 1.86 as the JIT crate does (workspace stays 1.85).
4. test262: a --only-temporal run selects every test under the Temporal directories or tagged Temporal/canonical-tz, against its own ratcheting baseline baselines/agnijs-temporal.json (expected failures listed), reported in README and run in CI. The main baseline is untouched until the final subtask folds Temporal into it.
5. Subtasks, in order, one commit each, test-first, pass counts recorded per subtask: (1) ADR, seam, feature, namespace, host clock hook, runner mode; (2) shared infrastructure and Temporal.Duration; (3) Instant, Now, Date.prototype.toTemporalInstant; (4) PlainDate and calendars; (5) PlainTime; (6) PlainDateTime; (7) PlainYearMonth and PlainMonthDay; (8) ZonedDateTime and time zones; (9) Intl integration (toLocaleString, DateTimeFormat with Temporal objects, intl402/Temporal); (10) fuzz target, security limits review, docs, default-on decision with size and startup numbers, fold into the main baseline.
6. Every built-in keeps the security rules: memory limit and string-length checks on results, interrupt polling in any native loop (Vm::poll), no recursion on script-controlled depth, no panics (temporal_rs errors map to RangeError/TypeError; the seam catches nothing it cannot prove).
<!-- SECTION:PLAN:END -->
