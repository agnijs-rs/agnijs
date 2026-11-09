---
id: TASK-103.8
title: Temporal
status: To Do
assignee: []
created_date: '2026-11-09T02:14:00Z'
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
