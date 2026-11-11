---
id: TASK-101.2
title: Heap and realm snapshots for startup
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-09T01:51:00Z'
updated_date: '2026-11-11T04:27:00Z'
labels:
  - needs-triage
dependencies:
  - TASK-101.1
  - TASK-100.12
parent_task_id: TASK-101
ordinal: 230000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Save and restore a realm or heap after initialization so startup becomes a memory load for every embedder (the UI-bundle startup case in the M9 benchmarks). Cover versioning against the bytecode container (ADR 0010), host objects and handles (ADR 0006), and the debugger.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A snapshot of an initialized realm (built-ins plus an embedder-run bootstrap script) can be written and restored, with a version mismatch rejected
- [ ] #2 Startup of a bundle from a snapshot is measured against the cold path with gable-js-bench; test262 and the host-driven tests pass with snapshot-created realms
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Engine::snapshot(realm) default method (Unsupported) plus a Snapshot error variant (Display, C API mapping).
2. agnijs-js-vm snapshot module: deterministic writer over objects/envs/strings/symbols/bigints/code reachable from the realm; natives by a per-process registry index (fingerprinted); refuse host objects/functions, natives with private state, generators, modules, pending jobs and other unsupported classes with an error.
3. Reader: AGJS container tag agnijs-snap-1 tied to the bytecode format, checksum, bounded counts, every id and payload validated; Vm::bare + make_engine_from_snapshot. Code blocks through the serializer once each; feedback and ICs empty; no JIT code.
4. Harden env access so a corrupt snapshot faults instead of panicking; bounded fuzz test.
5. Tests: tests/snapshot.rs through the Engine trait; host_driven in a snapshot variant; test262 --snapshot mode.
6. Bench: snapshot startup and bundle startup cold vs snapshot.
7. Docs (ADR/spec J11), backlog follow-ups for what is left.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
From TASK-101.1 (ADR 0019, J11; spec section 14): snapshot blockers in the current engine are NativeKind::Fn raw function pointers (need a native registry indexed by id), Rc<Cell<bool>>/Rc<Cell<usize>> state shared by NativeData closures (move into the heap or serialise with sharing), and HostObject with no serialisation hook (add one, or refuse with an error). Code blocks go through the existing serializer once each; inline-cache and feedback tables load empty; JIT code is never in a snapshot.
<!-- SECTION:NOTES:END -->
