---
id: TASK-101
title: 'AgniJS post-v1 performance tiers: snapshots, JIT and AOT'
status: To Do
assignee: []
created_date: '2026-11-09T01:51:00Z'
updated_date: '2026-11-09T02:04:00Z'
labels:
  - needs-triage
dependencies:
  - TASK-100.14
references:
  - docs/js-engine-spec.md
  - docs/adr/0004-gable-native-owns-its-javascript-engine-in-rust.md
ordinal: 228000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Post-v1 performance roadmap for AgniJS (the Gable JS engine after TASK-100.12 extracts and renames it): the tiers beyond the interpreter, aimed at general embedders and not only Gable. Starts only after the Fable performance fixes created by TASK-100.14 have landed, because a JIT built on a slow interpreter inherits its problems (TASK-100.13 baseline: 2.9x slower than QuickJS-NG on average, inline caches not helping, GC too lazy). Design constraints: the interpreter stays the portable baseline tier and the JIT is optional (a cargo feature or separate crate) so iOS, locked-down Android and WASM embedders keep a pure-Rust interpreter; the debugger (ADR 0017), hot update and bytecode versioning (ADR 0010) must keep working with every tier; unsafe stays confined to one small crate. Order: JIT-readiness constraints, snapshots, baseline JIT, optimizing tier, native AOT research. Spec section 2 non-goal 'a JIT' is a v1 non-goal only.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Subtasks 101.1 to 101.5 are done or consciously dropped, each with the evidence its own acceptance criteria ask for
- [ ] #2 The engine still passes test262 at the checked-in baselines with every tier enabled, and the interpreter remains the default and fallback
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Model assignment (maintainer, 2026-11-08): Fable agents do the performance fixes from TASK-100.14 and the JIT and AOT work in this task; other engine work goes to Opus.
<!-- SECTION:NOTES:END -->
