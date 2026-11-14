---
id: TASK-103.31
title: 'Rust embedding API: define what happens to an engine after a panic'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-12T06:25:00Z'
updated_date: '2026-11-14T09:42:00Z'
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
- [x] #1 The Rust API documents what an embedder may do with an engine after a panic unwinds out of it
- [x] #2 Either the engine refuses further calls after a panic (a poisoned state, as in the C API) or a test shows that it stays consistent after a panic in a host function
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Test first (crates/agnijs-js-vm/tests/panic.rs): a panic in a host function, in a Host hook and in a nested call back into the engine, caught by the embedder, leaves the engine refusing every later entry with Error::Poisoned and is_poisoned() true; a thrown error does not poison; a poisoned engine drops cleanly.
2. agnijs-engine-api: Error::Poisoned and Engine::is_poisoned (default false).
3. agnijs-js-vm: an unwind guard in Vm::enter (every entry that runs script or host code) sets a shared flag when std::thread::panicking() during its drop; enter, check_realm, snapshot and collect_garbage refuse a poisoned engine. The VM forbids unsafe code, so even the infallible accessors stay memory-safe, with unspecified results.
4. Map Error::Poisoned in the C API (AGNIJS_ERR_PANIC; the C API poisons its own state first anyway) and the VM's host-error-to-throw path.
5. Document it in the engine-api Engine docs, agnijs::Context and docs/security-model.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented as planned, with one change: Error::Poisoned maps to AGNIJS_ERR_POISONED in the C API (the status its later calls already return), not AGNIJS_ERR_PANIC. Enter also rechecks the flag after the closure returns, so a host function that catches its nested call's panic cannot carry on in a half-unwound engine; a host function's Err(Poisoned) ends the script as an interrupt does. collect_garbage does nothing once poisoned (it runs host objects' trace over roots that may be mid-update). The CLI's Inspected engine forwards is_poisoned. tests/panic.rs: 4 of 5 red before (the thrown-errors test passed, as it should). Verification: vm, engine-api, agnijs, agnijs-cli and quickjs suites (459 tests), C API with fault-injection (61), C API fuzz in release with 10x work, C API clippy -D warnings, header drift (cbindgen 0.29: no change), clippy on changed lines clean, test262 all 8 modes 0 regressions, benchmarks within 1% (see TASK-103.32).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
The Rust API now defines what happens after a panic: a guard on every VM entry that runs code (plus create_realm, snapshot and collect_garbage) poisons the engine when a panic unwinds through it, and every later call that runs code or touches a realm returns the new Error::Poisoned. Engine::is_poisoned and Context::is_poisoned report it, and dropping the engine is safe. Documented in the Engine and Context docs and in docs/security-model.md. Tests: tests/panic.rs (host function, Host hook, nested call, a panic caught inside a host function; errors do not poison).
<!-- SECTION:FINAL_SUMMARY:END -->
