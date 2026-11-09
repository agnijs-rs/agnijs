---
id: TASK-100.32
title: 'Engine: enforce no unsafe code outside the FFI and JIT crates'
status: Done
assignee:
  - '@fable-agent'
created_date: '2026-11-09T05:52:00Z'
updated_date: '2026-11-09T09:43:00Z'
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
- [x] #1 agnijs-js-parser, agnijs-js-compiler, agnijs-js-vm, agnijs-js-regex, agnijs-js-intl, agnijs-js-debugger, agnijs-js-cdp and agnijs-engine-api forbid unsafe code at the crate root, and the workspace builds and tests pass
- [x] #2 Crates that keep unsafe (the QuickJS-NG adapter, the benchmark memory probe) are listed with the reason in ADR 0019 or an addendum
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Commit d947463: `#![forbid(unsafe_code)]` at the crate root of agnijs-js-parser, -compiler, -vm, -regex, -intl, -debugger, -cdp and agnijs-engine-api. `cargo build --workspace` and `cargo test --workspace` pass. ADR 0019 (Context, the `unsafe` bullet) now lists the two crates that keep `unsafe` and why: agnijs-engine-quickjs (FFI into QuickJS-NG) and agnijs-js-bench (process-memory probe, `K32GetProcessMemoryInfo`/`getrusage`).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Every engine crate forbids unsafe code at the crate root; the workspace builds and tests pass. The QuickJS-NG adapter and the bench memory probe keep unsafe and are listed with reasons in ADR 0019.
<!-- SECTION:FINAL_SUMMARY:END -->
