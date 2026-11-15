---
id: TASK-104.3
title: Host function and host class contract
status: Done
assignee:
  - '@claude'
created_date: '2026-11-15T22:22:00Z'
updated_date: '2026-11-15T22:32:00Z'
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
- [x] #1 The rustdoc of HostFunction, CallContext, HostObject, Tracer and HostClassDef states argument and return conversion, how each Error a host function returns reaches JavaScript, how values held by host objects are rooted or traced, when finalizers run, and the re-entrancy rules
- [x] #2 Tests cover each documented rule: missing arguments, this and new_target, a thrown exception value, Host/Internal errors, uncatchable Interrupted/OutOfMemory/OutOfFuel, values held by a host object surviving collection and released on finalization, and re-entrant calls (call, eval, run_jobs, collect_garbage) from a host function
<!-- AC:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Rustdoc contract on HostFunction (arguments, return, error mapping per Error variant, panics poison, re-entrancy, Fn not FnMut), CallContext (fresh handles, realm is the function's realm, this unboxed, new_target always None on AgniJS), HostObject (ownership, held handles are roots, trace advisory and not called by AgniJS so cycles through host data leak, finalization from run_jobs with no engine, no finalizer at engine drop), Tracer, HostClassDef::finalize and the Engine host methods. crates/agnijs/tests/host_contract.rs (18 tests) covers each rule; collect_garbage from a host function is covered in lifetimes.rs. Tests: agnijs and agnijs-engine-api pass; rustdoc -D warnings clean.
<!-- SECTION:FINAL_SUMMARY:END -->
