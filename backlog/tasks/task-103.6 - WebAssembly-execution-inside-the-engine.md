---
id: TASK-103.6
title: WebAssembly execution inside the engine
status: To Do
assignee: []
created_date: '2026-11-09T02:14:00Z'
labels:
  - needs-triage
dependencies: []
parent_task_id: TASK-103
ordinal: 241000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Design study and, if justified, a WebAssembly interpreter inside the engine (MVP plus the stable proposals), exposing the WebAssembly JS API to complete the plugin-host story. Large; start with a design note on reuse (a pure-Rust wasm interpreter crate behind a seam, as with regress and ICU4X) versus building one, the WebAssembly spec testsuite as the gate, and sandbox and fuel integration. Model: Opus for the study and implementation.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A design note decides adopt versus build, scope and the spec-testsuite gate, and splits the work into tasks
- [ ] #2 If go: the WebAssembly JS API passes the agreed spec testsuite baseline, respects fuel, memory and interrupt limits, and keeps the engine pure Rust
<!-- AC:END -->
