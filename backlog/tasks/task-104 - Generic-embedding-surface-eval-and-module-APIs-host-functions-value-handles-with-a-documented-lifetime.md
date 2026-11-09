---
id: TASK-104
title: >-
  Generic embedding surface: eval and module APIs, host functions, value handles
  with a documented lifetime
status: To Do
assignee: []
created_date: '2026-11-09T05:07:00Z'
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
- [ ] #1 A written design (ADR) records which parts of the current Engine and Host traits become the public embedding API, which stay internal to adapters, and what changes, with the reasons drawn from the Gable adapter in the harness and the QuickJS-NG adapter
- [ ] #2 The lifetime story for Value and Handle is documented and tested: when a handle keeps a heap value alive, what reclaim_handles and collect_garbage guarantee, what happens to handles after an engine or realm is dropped, and that misuse is an error rather than undefined behaviour
- [ ] #3 The eval API (scripts, modules, bytecode), the job loop and the module loader hooks are documented with runnable examples in rustdoc, including error and exception handling and the interrupt hook
- [ ] #4 Host functions and host classes have a documented, tested contract: argument and return conversion, errors thrown from the host, tracing of values held by host objects for the collector, and re-entrancy rules
- [ ] #5 A semver and stability policy for the public crates is written down (which crates and items are public, what counts as breaking, how bytecode format changes are versioned) and a cargo-semver-checks or equivalent check runs in CI
- [ ] #6 An embedding guide and a minimal example crate (embed, run a script and a module, expose a host function, run the job loop) build and run in CI; a fuzzing corpus and a security disclosure policy (SECURITY.md) exist before the first external dependent
<!-- AC:END -->
