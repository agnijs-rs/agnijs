---
id: TASK-101.5
title: Native AOT compilation research
status: To Do
assignee: []
created_date: '2026-11-09T01:52:00Z'
updated_date: '2026-11-09T05:53:00Z'
labels:
  - needs-triage
dependencies:
  - TASK-101.1
  - TASK-100.12
parent_task_id: TASK-101
ordinal: 233000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Research track: ahead-of-time compilation of JavaScript to native code at build time for a typed or restricted subset, with fallback to the interpreter for dynamic code. The only route to speed where a JIT is forbidden (iOS). Study static Hermes and Porffor; produce a feasibility report and a prototype scope, not a product.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A research note in docs/ compares the options with prior art, estimates cost and payoff, and recommends go, no-go or defer
- [ ] #2 If go, a prototype scope and follow-up tasks are written
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Model assignment (maintainer, 2026-11-08): Opus.

From TASK-101.1 (ADR 0019): AOT should reuse the baseline JIT's code generator (Cranelift via cranelift-object) and its extern C runtime-helper contract (J10), produce a separate artefact with its own format tag bound to the engine build, and resume in the interpreter (same frame layout, J1) for anything it does not compile.
<!-- SECTION:NOTES:END -->
