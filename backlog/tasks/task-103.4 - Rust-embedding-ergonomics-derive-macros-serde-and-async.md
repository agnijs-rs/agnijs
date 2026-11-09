---
id: TASK-103.4
title: 'Rust embedding ergonomics: derive macros, serde and async'
status: To Do
assignee: []
created_date: '2026-11-09T02:14:00Z'
updated_date: '2026-11-09T02:35:00Z'
labels:
  - needs-triage
dependencies: []
parent_task_id: TASK-103
ordinal: 239000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
First-ten-minutes embedding experience in Rust: derive macros for host classes and functions, serde interop for passing values across the boundary, Rust async functions callable as JS promises (and the reverse), typed errors, and a cookbook. Design note first, comparing Boa, rquickjs and deno_core ergonomics. Model: Opus.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A design note compares existing Rust embedding APIs and chooses the surface; a derive-based host class and a serde round trip are implemented over the handle-based interface
- [ ] #2 A Rust async function can be exposed to JS as a promise-returning function and awaited from the engine, with tests including cancellation and errors
- [ ] #3 A cookbook in docs shows an embedder going from cargo add to calling into JS in under ten lines
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Model assignment (maintainer, 2026-11-08): Opus for the derive macros, serde and async design; Sonnet for the cookbook and embedding guide once the API exists.
<!-- SECTION:NOTES:END -->
