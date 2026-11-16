---
id: TASK-103.8.10
title: 'Temporal: fuzzing, limits review, docs and default-on'
status: To Do
assignee:
  - '@claude'
created_date: '2026-11-16T01:48:00Z'
labels:
  - temporal
dependencies: []
parent_task_id: TASK-103.8
ordinal: 339000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Temporal parses untrusted strings and does calendar arithmetic on script-controlled values, so it needs a fuzz target and a review against the security model before it is on by default.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 agnijs-fuzz has a Temporal target
- [ ] #2 security-model.md, determinism.md, binary-size.md and README cover Temporal
- [ ] #3 The temporal feature is on by default, or the ADR says why not, with size and startup measured
- [ ] #4 Temporal tests are in the main baseline and post-es2025-features.txt no longer lists Temporal
<!-- AC:END -->
