---
id: TASK-101.1
title: JIT-readiness constraints and ADR
status: To Do
assignee: []
created_date: '2026-11-09T01:51:00Z'
updated_date: '2026-11-09T03:29:00Z'
labels:
  - needs-triage
dependencies:
  - TASK-100.14
parent_task_id: TASK-101
ordinal: 229000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Record the constraints that keep a later JIT possible, as an ADR plus a short section in the engine spec, and check the current engine against them. Constraints to cover: frame layout and register mapping, stable per-site inline-cache feedback (shape metadata readable by a JIT), GC roots and write barriers a JIT can emit (stack maps or conservative scanning), explicit side-effect and exception boundaries for deoptimization, and a public embedding API that does not lock the engine to interpretation. Engine crates stay Gable-agnostic (ADR 0016).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 An ADR in docs/adr records the JIT-readiness constraints and the tiering and deoptimization strategy, including the optional-feature decision and the platform story (macOS entitlement, iOS, locked-down Android, WASM)
- [ ] #2 The spec has a short section listing the constraints; every existing engine design that violates one is listed with a proposed fix, created as backlog tasks where worth doing
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Model assignment (maintainer, 2026-11-08): Opus.

Ordering (maintainer, 2026-11-08): this ADR gates TASK-100.17 and TASK-100.18 so the dispatch and bytecode rewrites avoid choices that block a JIT. It is cheap and stays ahead of the engine extraction. The rest of TASK-101 follows the extraction (TASK-100.12).
<!-- SECTION:NOTES:END -->
