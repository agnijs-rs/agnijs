---
id: TASK-103.12
title: no_std and small-target build
status: To Do
assignee: []
created_date: '2026-11-09T02:15:00Z'
labels:
  - needs-triage
dependencies: []
parent_task_id: TASK-103
ordinal: 247000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Investigate a no_std plus alloc build of the core engine for microcontrollers and small WASM targets, listing every std dependency (time, threads, OS), the host hooks needed to replace them, and the cost. Study first; implement if the cost is low. Model: Sonnet for the investigation, Opus for implementation.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A report lists the std dependencies of each engine crate and the work to make a no_std plus alloc build, with a go or no-go recommendation
- [ ] #2 If go: the core crates build with no_std plus alloc in CI and pass a documented test subset
<!-- AC:END -->
