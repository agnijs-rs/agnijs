---
id: TASK-103.31
title: 'Rust embedding API: define what happens to an engine after a panic'
status: To Do
assignee: []
created_date: '2026-11-12T06:25:00Z'
labels:
  - ready-for-agent
  - security
dependencies: []
references:
  - docs/security-model.md
parent_task_id: TASK-103
priority: low
ordinal: 295000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the security review (TASK-103.9, docs/security-model.md). The C API catches every panic and poisons the engine it happened in (ADR 0021, `agnijs-capi/src/guard.rs`). The Rust API (`agnijs::Context`, `Engine` from `agnijs_js_vm::make_engine`) has no such boundary: a panic in the engine, or in a host function or `Host` hook written by the embedder, unwinds through the VM to the embedder, and if the embedder catches it and keeps using the engine, the VM's invariants (frame stack, native depth, roots, the attention word) may no longer hold. Nothing documents whether that is allowed. Embedders that run untrusted code need to know whether a caught panic means discarding the engine.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 The Rust API documents what an embedder may do with an engine after a panic unwinds out of it
- [ ] #2 Either the engine refuses further calls after a panic (a poisoned state, as in the C API) or a test shows that it stays consistent after a panic in a host function
<!-- AC:END -->
