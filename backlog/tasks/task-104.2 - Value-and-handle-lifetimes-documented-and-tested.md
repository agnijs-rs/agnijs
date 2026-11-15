---
id: TASK-104.2
title: 'Value and handle lifetimes: documented and tested'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-15T22:22:00Z'
updated_date: '2026-11-15T22:30:00Z'
labels: []
dependencies: []
parent_task_id: TASK-104
type: feature
ordinal: 325000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Embedders hold Handles in their own structures for a long time (ADR 0006). What keeps a heap value alive, when slots are released, what collect_garbage does, and what happens to handles that outlive their engine is only partly written down and only the release path is tested.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 The rustdoc of Handle, Value, reclaim_handles and collect_garbage states when a handle keeps a value alive, what reclaim_handles and collect_garbage guarantee (including when called from inside a host function), what happens to handles after the engine is dropped, and what a RealmId is valid for
- [x] #2 Tests show: a held handle keeps its object alive across collections; dropping it lets the object be collected; handles survive and drop safely after their engine is gone; a handle given to another engine or an unknown realm id is an error, not a panic
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Tests first (crates/agnijs/tests/lifetimes.rs): held handle keeps value alive, last clone dropped lets it be collected, reclaim timing, collect_garbage inside a host function, handles after engine drop, foreign handle and unknown realm errors, handles across realms. 2. Document the contract in agnijs-engine-api value.rs (Lifetimes section), Handle, RealmId, reclaim_handles, live_handles and collect_garbage; compile_fail doctest that Handle is not Send.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Behaviour verified before documenting: values handed out are new handles per crossing (== compares slots); dropping the engine drops host data without running finalizers; the VM answers type_of/is_array/strict_equals on a foreign handle as for undefined; run_jobs on an unknown realm is UnknownRealm. Fixed an unresolved intra-doc link to conformance_tests! that cargo doc -D warnings reported.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Wrote the lifetime contract into agnijs-engine-api (a Lifetimes section in the value module, plus Handle, RealmId, reclaim_handles, live_handles and collect_garbage docs and a compile_fail doctest that Handle is not Send) and tested it in crates/agnijs/tests/lifetimes.rs (10 tests: rooting, release on last drop, reclaim timing, collect_garbage from a host function, handles after engine drop, foreign handles, unknown realms, handles across realms). cargo test -p agnijs --test lifetimes, cargo test -p agnijs-engine-api --doc and cargo doc -D warnings pass. RealmId stays untagged (owner decision 4 in TASK-104).
<!-- SECTION:FINAL_SUMMARY:END -->
