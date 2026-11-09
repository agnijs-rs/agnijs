---
id: TASK-101.2
title: Heap and realm snapshots for startup
status: To Do
assignee: []
created_date: '2026-11-09T01:51:00Z'
updated_date: '2026-11-09T03:29:00Z'
labels:
  - needs-triage
dependencies:
  - TASK-101.1
  - TASK-100.12
parent_task_id: TASK-101
ordinal: 230000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Save and restore a realm or heap after initialization so startup becomes a memory load for every embedder (the UI-bundle startup case in the M9 benchmarks). Cover versioning against the bytecode container (ADR 0010), host objects and handles (ADR 0006), and the debugger.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A snapshot of an initialized realm (built-ins plus an embedder-run bootstrap script) can be written and restored, with a version mismatch rejected
- [ ] #2 Startup of a bundle from a snapshot is measured against the cold path with gable-js-bench; test262 and the host-driven tests pass with snapshot-created realms
<!-- AC:END -->
