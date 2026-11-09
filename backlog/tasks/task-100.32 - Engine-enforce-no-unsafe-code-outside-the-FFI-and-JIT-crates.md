---
id: TASK-100.32
title: 'Engine: enforce no unsafe code outside the FFI and JIT crates'
status: To Do
assignee: []
created_date: '2026-11-09T05:52:00Z'
labels:
  - needs-triage
dependencies: []
documentation:
  - docs/adr/0019-jit-readiness-constraints-tiering-and-deoptimization.md
parent_task_id: TASK-100
ordinal: 281000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
ADR 0009 records that no engine crate needs unsafe, and ADR 0019 (J10) makes a future agnijs-js-jit crate the only engine crate allowed it, besides embedder-facing FFI crates (the QuickJS-NG adapter, the C API of TASK-103.3). Nothing enforces this today: no crate sets #![forbid(unsafe_code)], so an interpreter rewrite such as unchecked register access in TASK-100.17 could add unsafe to agnijs-js-vm without anyone noticing. Found while checking the engine against ADR 0019 (TASK-101.1).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 agnijs-js-parser, agnijs-js-compiler, agnijs-js-vm, agnijs-js-regex, agnijs-js-intl, agnijs-js-debugger, agnijs-js-cdp and agnijs-engine-api forbid unsafe code at the crate root, and the workspace builds and tests pass
- [ ] #2 Crates that keep unsafe (the QuickJS-NG adapter, the benchmark memory probe) are listed with the reason in ADR 0019 or an addendum
<!-- AC:END -->
