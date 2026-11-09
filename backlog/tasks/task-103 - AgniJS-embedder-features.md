---
id: TASK-103
title: AgniJS embedder features
status: To Do
assignee: []
created_date: '2026-11-09T02:12:00Z'
labels:
  - needs-triage
dependencies:
  - TASK-100.12
references:
  - docs/js-engine-spec.md
  - docs/adr/0016-the-engine-workspace-stands-alone.md
ordinal: 235000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Features that make AgniJS (the Gable JS engine after TASK-100.12 extracts and renames it) a better choice for embedders in general, not only Gable. Each subtask starts with a short design note or ADR before code. The engine stays Gable-agnostic (ADR 0016). Related work already filed: heap and realm snapshots (TASK-101.2), the linear-time regex matcher (TASK-102), JIT and AOT (TASK-101). Model policy (maintainer, 2026-11-08): Opus for implementation, Sonnet for research, documentation and measurement, Fable only where it truly matters (here: the shareable-heap and isolates design in 103.11, plus performance, JIT and AOT elsewhere).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Every subtask is done or consciously dropped with its own evidence
- [ ] #2 AgniJS has a published embedding guide that covers resource limits, determinism, security model, sizes and the C and Rust APIs
<!-- AC:END -->
