---
id: TASK-104.3
title: Host function and host class contract
status: To Do
assignee:
  - '@claude'
created_date: '2026-11-15T22:22:00Z'
labels: []
dependencies: []
parent_task_id: TASK-104
type: feature
ordinal: 326000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Host functions and host classes are how an embedder exposes Rust to JavaScript. Their argument and return conversion, the error mapping of what a host function returns, how values held by host objects are kept alive or traced, and what a host function may call back into are scattered or unstated. The AgniJS collector never calls HostObject::trace, which the trait docs do not say.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 The rustdoc of HostFunction, CallContext, HostObject, Tracer and HostClassDef states argument and return conversion, how each Error a host function returns reaches JavaScript, how values held by host objects are rooted or traced, when finalizers run, and the re-entrancy rules
- [ ] #2 Tests cover each documented rule: missing arguments, this and new_target, a thrown exception value, Host/Internal errors, uncatchable Interrupted/OutOfMemory/OutOfFuel, values held by a host object surviving collection and released on finalization, and re-entrant calls (call, eval, run_jobs, collect_garbage) from a host function
<!-- AC:END -->
