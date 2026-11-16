---
id: TASK-103.13.3
title: 'TypeScript: fuzz target and documentation'
status: To Do
assignee:
  - '@claude'
created_date: '2026-11-16T16:50:00Z'
labels:
  - typescript
dependencies: []
parent_task_id: TASK-103.13
ordinal: 349000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The transpiler takes source an embedder may not trust, so it is fuzzed like the other front ends, and embedders need the documentation to use it.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 agnijs-fuzz has a typescript target behind a typescript feature, run by the fuzz CI job and the smoke test, with a cargo-fuzz target in fuzz/
- [ ] #2 README, stability, security model, determinism, embedding guide and CHANGELOG describe the crate, its limits and its policy
<!-- AC:END -->
