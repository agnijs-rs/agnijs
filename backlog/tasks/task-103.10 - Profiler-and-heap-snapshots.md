---
id: TASK-103.10
title: Profiler and heap snapshots
status: To Do
assignee: []
created_date: '2026-11-09T02:15:00Z'
labels:
  - needs-triage
dependencies: []
parent_task_id: TASK-103
ordinal: 245000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Profiling and memory tooling for long-running embedders: a sampling CPU profiler with source-mapped output, allocation counters, and heap snapshots in a format Chrome DevTools can open (through the CDP adapter, ADR 0017). The spec lists profilers as a later item. Model: Opus.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A CPU profile can be captured from the host API and over CDP and opened in Chrome DevTools, with source-mapped names
- [ ] #2 A heap snapshot can be captured and opened in DevTools; overhead with profiling disabled is within noise on gable-js-bench
<!-- AC:END -->
