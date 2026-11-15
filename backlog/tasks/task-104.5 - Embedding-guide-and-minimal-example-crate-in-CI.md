---
id: TASK-104.5
title: Embedding guide and minimal example crate in CI
status: To Do
assignee:
  - '@claude'
created_date: '2026-11-15T22:22:00Z'
labels: []
dependencies: []
parent_task_id: TASK-104
type: docs
ordinal: 328000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
A first external embedder needs one page that walks the whole surface and a crate it can copy. The cookbook covers the Context layer only.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 docs/embedding-guide.md walks through embedding: construct an engine, run a script and a module, expose a host function and a host class, drive the job loop, limits and errors, handle lifetimes, with links to the cookbook, the security model, SECURITY.md and the fuzzing corpus
- [ ] #2 A minimal example crate in the workspace embeds the engine, runs a script and a module, exposes a host function and runs the job loop; CI builds and runs it and checks its output
<!-- AC:END -->
