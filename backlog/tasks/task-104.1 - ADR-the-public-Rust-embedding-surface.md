---
id: TASK-104.1
title: 'ADR: the public Rust embedding surface'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-15T22:22:00Z'
updated_date: '2026-11-15T22:29:00Z'
labels: []
dependencies: []
parent_task_id: TASK-104
type: docs
ordinal: 324000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
A first external embedder needs to know which items it may build on. Today the engine interface crate mixes embedder items with items only engine implementations need (HandleRegistry, next_engine_id, the bytecode container helpers, the conformance kit), and embedders reach into agnijs-js-vm, whose pub surface is incidental, to construct an engine. Record the decision from what the Gable adapter and the QuickJS-NG adapter actually used.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 An ADR in docs/adr says which parts of the Engine and Host traits and their types are the public embedding API, which are implementor-only, and what changes, with reasons from the Gable harness adapter and the QuickJS-NG adapter
- [x] #2 Embedders can construct an AgniJS engine (fresh or from a snapshot) through the agnijs crate without depending on agnijs-js-vm
- [x] #3 Implementor-only items are marked so they are outside the semver-checked surface, and the error enums that keep growing can gain variants without a breaking change
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Survey what the Gable harness and the QuickJS-NG adapter use. 2. Test-first: agnijs::new_engine / new_engine_from_snapshot (tests/surface.rs). 3. ADR 0026. 4. non_exhaustive on api::Error and agnijs::Error with wildcard arms in the VM and the C API. 5. doc(hidden) on HandleRegistry, next_engine_id, bytecode, conformance.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Gable harness (native/engine-harness at pin fa65c08) uses only public calling-side Engine methods, six Host hooks and agnijs_js_vm::make_engine; QuickJS adapter is the only external user of HandleRegistry, next_engine_id, bytecode helpers and the conformance kit. Workspace builds with --all-targets after the non_exhaustive change; wildcard arms added in agnijs-capi registry.rs (AGNIJS_ERR_ENGINE) and agnijs-js-vm engine.rs (throws Error with message). Owner decisions recorded in TASK-104 notes (RealmId tagging, non_exhaustive hook enums, agnijs-js-resolve and debugger under the policy).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
ADR 0026 makes the agnijs crate the one dependency of an embedder, with two public layers (Context and agnijs::api), lists the public and implementor-only items of agnijs-engine-api, and records what stays (lifetime-free handles, untagged RealmId, advisory trace). Added agnijs::new_engine and new_engine_from_snapshot (tests/surface.rs, 4 tests, plus 2 doctests), made api::Error and agnijs::Error non_exhaustive, and hid the implementor items with doc(hidden). Workspace builds with all targets; agnijs doctests pass.
<!-- SECTION:FINAL_SUMMARY:END -->
