---
id: TASK-103.8.1
title: 'Temporal: ADR, seam crate, feature flag, namespace and test262 mode'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-16T01:48:00Z'
updated_date: '2026-11-16T02:23:00Z'
labels:
  - temporal
dependencies: []
parent_task_id: TASK-103.8
ordinal: 330000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Temporal needs a recorded adopt-versus-build decision and a place for the dependency before any built-in can land, and the test262 harness needs a way to measure Temporal without disturbing the main zero-regression gate.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 ADR 0027 records the options (temporal_rs, building on ICU4X, jiff) and the choice, and the license check passes
- [x] #2 agnijs-js-temporal wraps temporal_rs so no temporal_rs type appears in its public API
- [x] #3 The temporal feature of agnijs-js-vm, off by default, installs a Temporal namespace object; the default build is unchanged
- [x] #4 Host has a nanosecond clock hook defaulting to now_ms, documented in determinism.md
- [x] #5 test262 run --only-temporal runs the Temporal tests against baselines/agnijs-temporal.json and CI runs it
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. ADR 0027: options (temporal_rs, build on ICU4X icu_calendar plus our own tz code, jiff), decision temporal_rs 0.2 with timezone_provider CompiledTzdbProvider over jiff-tzdb (shared with Intl), seam rules, feature off by default, MSRV 1.86 for the seam crate.
2. crates/agnijs-js-temporal: dependency on temporal_rs without default features (no sys, no iana-time-zone), a process-wide provider, own error type (Range/Type + message). Start with what the namespace needs; types grow per subtask.
3. agnijs-js-vm feature temporal = dep:agnijs-js-temporal; builtins/temporal/mod.rs installs the Temporal namespace (toStringTag "Temporal") on the global; snapshot fingerprint includes the feature.
4. Host::now_ns (i128 epoch nanoseconds) defaulting to now_ms scaled; docs/determinism.md row.
5. test262: --only-temporal (Temporal dirs plus tests tagged Temporal or canonical-tz, both features allowed), baseline agnijs-temporal.json, runner and fuzz crates enable the feature; CI step; README row.
6. Tests first: VM test that Temporal exists with the right tag under the feature; runner unit test for the selection.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Verified: license check passes (199 packages); clippy -D warnings clean on the touched files (pre-existing lints elsewhere in agnijs-js-vm and the test262 runner are not from this change); cargo test of agnijs-engine-api, agnijs-js-temporal, agnijs-js-test262, agnijs-fuzz and agnijs-js-vm: 55 test binaries, 0 failures; agnijs-js-vm --features temporal --test temporal passes. First --only-temporal measurement: 48 / 5168 (built-ins 39/4611, intl402 9/557); 1560 skipped for Intl.Era-monthcode and 8 for Intl.DurationFormat, both outside the feature cut.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
temporal_rs 0.2.6 adopted behind the new agnijs-js-temporal seam (ADR 0027), with the IANA data and ICU4X calendar data Intl already links. agnijs-js-vm gains a temporal feature (off by default; the runner and fuzz crate enable it) that installs the Temporal namespace, Temporal intrinsics, and a snapshot fingerprint bit. Host::now_ns (default now_ms scaled) is the Temporal.Now clock, documented in determinism.md and security-model.md. test262 run --only-temporal measures Temporal against baselines/agnijs-temporal.json in CI; first count 48/5168. Default build unchanged.
<!-- SECTION:FINAL_SUMMARY:END -->
