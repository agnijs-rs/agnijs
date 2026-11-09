---
id: TASK-103.21
title: Hot module replacement
status: To Do
assignee: []
created_date: '2026-11-09T05:17:00Z'
labels:
  - needs-triage
dependencies: []
parent_task_id: TASK-103
ordinal: 277000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Replace a running module with new code while keeping state, useful for UI engines such as Gable Native and for live-reload tooling. Define the host API (module versioning, dependent re-linking, accept and dispose hooks, state handover), build it on the module loader, and keep the engine free of Gable specifics (ADR 0016). Model: Opus.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A short ADR defines the semantics and the host API for replacing a module, including dependents, live bindings and errors during replacement
- [ ] #2 A working implementation passes tests that replace modules at runtime without losing state, and a failed replacement leaves the old module running
<!-- AC:END -->
