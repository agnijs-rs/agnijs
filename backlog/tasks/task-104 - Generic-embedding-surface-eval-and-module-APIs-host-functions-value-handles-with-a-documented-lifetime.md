---
id: TASK-104
title: >-
  Generic embedding surface: eval and module APIs, host functions, value handles
  with a documented lifetime
status: Done
assignee:
  - '@claude'
created_date: '2026-11-09T05:07:00Z'
updated_date: '2026-11-16T01:23:00Z'
labels:
  - needs-triage
dependencies:
  - TASK-100.12
references:
  - crates/agnijs-engine-api/src
documentation:
  - docs/adr/0006-engine-interface-is-handle-based-and-host-driven.md
  - docs/js-engine-spec.md
type: feature
ordinal: 265000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The engine is an embeddable library, but its only embedding surface today is the engine interface in agnijs-engine-api, which landed during M7 to M9 shaped by Gable's adapter and the QuickJS-NG adapter: an Engine trait with realm-scoped eval_script, load_module, run_jobs, compile_script/compile_module/eval_bytecode, memory limit and usage, value construction and property access, call and construct, promise state, host functions (new_function with Rc<HostFunction>), host classes and objects (register_host_class, new_host_object, HostObject with trace), and persistent reference-counted Handle values with no lifetime parameter; plus a Host trait (resolve_module, load_module, import_meta, promise_rejection, can_compile_strings, console, interrupt, now_ms, time zone and locale hooks, regex_matcher). Every call takes &mut self and a RealmId, and handles are reclaimed by reclaim_handles. That is a good internal boundary, but a first external embedder needs a documented, stable, ergonomic API and a policy for changing it. This task designs that surface from what the adapter actually needed, so it is neither speculative nor Gable-shaped (ADR 0016: nothing Gable-specific). Overlaps in part with TASK-103.3 (C API) and TASK-103.4 (Rust ergonomics); this task decides the base layer they build on and should land first or at least settle the handle and lifetime model they share.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A written design (ADR) records which parts of the current Engine and Host traits become the public embedding API, which stay internal to adapters, and what changes, with the reasons drawn from the Gable adapter in the harness and the QuickJS-NG adapter
- [x] #2 The lifetime story for Value and Handle is documented and tested: when a handle keeps a heap value alive, what reclaim_handles and collect_garbage guarantee, what happens to handles after an engine or realm is dropped, and that misuse is an error rather than undefined behaviour
- [x] #3 The eval API (scripts, modules, bytecode), the job loop and the module loader hooks are documented with runnable examples in rustdoc, including error and exception handling and the interrupt hook
- [x] #4 Host functions and host classes have a documented, tested contract: argument and return conversion, errors thrown from the host, tracing of values held by host objects for the collector, and re-entrancy rules
- [x] #5 A semver and stability policy for the public crates is written down (which crates and items are public, what counts as breaking, how bytecode format changes are versioned) and a cargo-semver-checks or equivalent check runs in CI
- [x] #6 An embedding guide and a minimal example crate (embed, run a script and a module, expose a host function, run the job loop) build and run in CI; a fuzzing corpus and a security disclosure policy (SECURITY.md) exist before the first external dependent
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. TASK-104.1 ADR 0026 and the surface changes it records. 2. TASK-104.2 handle and realm lifetime docs and tests. 3. TASK-104.3 host function and host class contract. 4. TASK-104.4 runnable rustdoc examples (eval, modules, bytecode, jobs, errors, interrupt). 5. TASK-104.5 embedding guide and example crate in CI. 6. TASK-104.6 semver policy and cargo-semver-checks in CI. One commit per subtask; gates before push per the brief.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Assessment against what exists at d411c1d (2026-11-15):

- AC1 (ADR): nothing. agnijs-engine-api is the full interface (Engine, Host, Value/Handle, errors) and `agnijs` (TASK-103.4) is an ergonomic Context over it that re-exports it as `agnijs::api`. Embedders also reach `agnijs_js_vm::make_engine` directly; the VM crate has a large incidental pub surface. Implementor-only items (HandleRegistry, next_engine_id, bytecode::wrap/unwrap, conformance) sit beside embedder items with no marking. Needs an ADR.
- AC2 (lifetimes): partly documented (value.rs module docs, reclaim_handles doc, ADR 0006). Not documented: what collect_garbage guarantees (it reclaims dropped handles first, and only defers when called from inside the engine), handles after the engine is dropped (drop is harmless, use on another engine is ForeignHandle), realms (no realm drop exists; RealmId is an untagged index). Tests exist only for release (conformance handles_are_released_when_dropped). Needs docs and a test file.
- AC3 (rustdoc examples): only the crate-level 3-line example in agnijs and the cookbook (Context-level, no modules, bytecode, interrupt). Needs runnable examples on the Engine/Host items.
- AC4 (host function/class contract): partly in doc comments. Gap found: HostObject::trace is never called by the AgniJS collector (gc.rs: host objects reference JS values only through handles, which are roots), so a cycle through a host object is never collected. Contract must say so; tests needed for conversion, thrown errors, uncatchable errors, tracing/rooting, re-entrancy.
- AC5 (semver policy + CI check): stability.md defers it here. No semver tooling in CI.
- AC6 (guide + example crate, fuzz corpus, SECURITY.md): SECURITY.md and the fuzz targets with seeds (crates/agnijs-fuzz, fuzz/) exist from TASK-103.18. The cookbook covers Context only. No example crate.

Plan: split into six subtasks, one commit each, test-first where there is behaviour:
1. TASK-104.1 ADR 0026 (public surface, two layers: `agnijs` front door + `agnijs::api`; implementor items hidden from the semver surface) and the code changes it records.
2. TASK-104.2 Handle/Value/realm lifetime docs and tests.
3. TASK-104.3 Host function and host class contract docs and tests.
4. TASK-104.4 Runnable rustdoc examples for eval, modules and loader hooks, bytecode, job loop, errors, interrupt.
5. TASK-104.5 Embedding guide and a minimal example crate run in CI.
6. TASK-104.6 Semver policy in docs/stability.md and a cargo-semver-checks CI job.

Owner decisions (recommendation and the default taken; none is hard to reverse):

1. Public surface = the `agnijs` crate (Context + `agnijs::api`), ADR 0026. Default taken. Reversible: the interface crate stays usable directly.
2. `#[non_exhaustive]` on `api::Error` and `agnijs::Error`. Recommended and taken: Error gained OutOfFuel and Poisoned this month. Costs a wildcard arm in the VM, C API and CLI matches. Removing it later is not breaking.
3. `#[non_exhaustive]` on the hook enums (ModuleSource, ConsoleLevel, RejectionEvent, RegexMatcher, ErrorKind). Recommend doing it before the first release; NOT done (matched in 8 workspace crates; adding later is breaking only after a release).
4. Tag RealmId with its engine, like Handle, so another engine's realm id is UnknownRealm instead of naming a realm with the same index. Recommend before 1.0; NOT done (changes a type every call takes). Documented as the contract for now.
5. HostObject::trace is advisory; AgniJS treats handles in host data as roots, so a cycle through host data leaks until the engine is dropped. Recommend keeping it (making traced handles edges needs strong-count bookkeeping and risks freeing a value a host clone still names). Documented and tested; implementing edges later is additive.
6. agnijs-js-resolve, the debugger capability and agnijs-js-cdp: embedder-facing but outside the semver policy until reviewed. Default taken.
7. Implementor-only items (HandleRegistry, next_engine_id, bytecode, conformance) are `#[doc(hidden)]`, outside the semver policy; implementing Engine outside the workspace is unsupported. Default taken.

Further owner decisions (TASK-104.6; defaults taken, none hard to reverse):
8. Version bumps before 1.0 happen per change: the semver CI job compares with the base of each push/PR, so the commit that breaks agnijs or agnijs-engine-api bumps the workspace minor version (this push: 0.1.0 -> 0.2.0, for the non_exhaustive Errors and the hidden implementor items). Alternative: compare with the last release tag and bump at release time; needs a first release. Recommend keeping per-change bumps until publishing.
9. Engine is not sealed, so a new Engine method without a default is breaking to cargo-semver-checks. Default: new methods get a default body or the bump. Recommend sealing Engine (doc(hidden) supertrait implemented by the VM and the QuickJS-NG adapter) before 1.0.
10. MSRV: raising it is not breaking but never in a patch release; before 1.0 it bumps the minor version; a new MSRV is at least six months old. No MSRV CI job yet (kept by review); recommend adding one before publishing.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Done through subtasks 104.1-104.6. ADR 0026 makes the agnijs crate (Context plus agnijs::api) the public embedding surface, with new_engine/new_engine_from_snapshot; Error enums are non_exhaustive and implementor items doc(hidden). The value/handle lifetime contract (value module rustdoc, tests/lifetimes.rs) and the host function/host object contract (HostFunction, CallContext, HostObject rustdoc, tests/host_contract.rs) are documented and tested. Engine/Host methods have runnable rustdoc examples covering exceptions, syntax errors, module load failure, BytecodeMismatch and interrupts. docs/embedding-guide.md (doctested) and examples/embed-minimal (output checked in CI). docs/stability.md states the semver policy; the CI semver job runs cargo-semver-checks against the base of each push/PR; workspace bumped to 0.2.0 for the breaking changes. SECURITY.md and the fuzz seed corpus (crates/agnijs-fuzz/src/seeds.rs) exist; SECURITY.md still has the owner's TODO contact address (TASK-103.18). Verified: workspace tests, clippy on touched crates, test262 default/precompile/snapshot/intl402/QuickJS-NG 0 regressions, GC-stress slice, C API fault injection/fuzz/header drift, agnijs-fuzz all --seed 1, CI run 43774584750 green (all 26 jobs). Open owner decisions are in the notes (hook enums non_exhaustive, sealing Engine, tagging RealmId, MSRV rule and CI).
<!-- SECTION:FINAL_SUMMARY:END -->
