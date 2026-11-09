---
id: TASK-103.23
title: Async runtime integration for Rust embedders
status: To Do
assignee: []
created_date: '2026-11-09T05:17:00Z'
labels:
  - needs-triage
dependencies: []
parent_task_id: TASK-103
ordinal: 279000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Helpers that connect host promises and timers to common Rust executors (tokio first): awaiting a JavaScript promise from async Rust, resolving JavaScript promises from Rust futures, and a ready-made event loop that drives jobs, timers and host callbacks. Coordinate with TASK-103.4 (embedding ergonomics) so the two do not overlap. Model: Opus.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A crate offers tokio integration with an example, and tests cover cancellation and errors
- [ ] #2 The design notes record how it relates to TASK-103.4 and to the engine job queue
<!-- AC:END -->
