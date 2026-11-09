---
id: TASK-103.22
title: 'Bindings built on the C API: Python, Node addon and .NET'
status: To Do
assignee: []
created_date: '2026-11-09T05:17:00Z'
labels:
  - needs-triage
dependencies: []
parent_task_id: TASK-103
ordinal: 278000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Language bindings over the C API (TASK-103.3) so non-Rust projects can embed AgniJS: a Python package, a Node native addon and a .NET wrapper. The no-panic rule for the C API applies to all of them. Decide which ship first and what lives in this repository versus separate repositories. The Node addon publishes under the npm organization agnijs-rs (maintainer created it), for example @agnijs-rs/node, with prebuilt binaries per platform; publishing needs the maintainer credentials. Model: Sonnet for each binding, Opus for the shared design.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 An ADR picks the order and the repository layout for the bindings
- [ ] #2 Each shipped binding has a test suite in CI and a working example
<!-- AC:END -->
