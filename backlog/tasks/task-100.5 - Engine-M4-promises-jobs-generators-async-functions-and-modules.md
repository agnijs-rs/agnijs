---
id: TASK-100.5
title: 'Engine M4: promises, jobs, generators, async functions and modules'
status: Done
assignee: []
created_date: '2026-11-07T22:36:00Z'
updated_date: '2026-11-08T16:45:00Z'
labels:
  - ready-for-agent
dependencies:
  - TASK-100.4
documentation:
  - docs/js-engine-spec.md
parent_task_id: TASK-100
ordinal: 206000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Gable apps are async and modular; HMR depends on module re-loading. Spec sections 3, 5 and 7.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Promise jobs run only through the host-driven job queue, and rejections reach the promise-rejection tracker
- [x] #2 Iterators, generators, async functions, async generators, static and dynamic modules, top-level await and import.meta (host hook) pass their test262 directories or are excluded with reasons
- [x] #3 A module can be re-loaded under a new specifier and its old record is collected once unreachable, covered by a test
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Most of M4's machinery (promise jobs on `Vm::jobs`, the host-driven drain, the rejection tracker, resumable-frame generators and async functions, static and dynamic modules, top-level await, `import.meta` through the host hook) landed in M2 because the language tests needed it; M4 closed the remaining gaps and verified the acceptance criteria.

AC #1: the only caller of the VM's job drain is `Engine::run_jobs` (`crates/gable-js-vm/src/engine.rs`); nothing inside the VM runs jobs on its own, and the conformance kit's `jobs_run_only_when_the_host_drains_them` and `rejections_reach_the_tracker` pass on both adapters.

AC #2: fixed AsyncModuleExecutionRejected ordering (a module's own top-level promise rejects before its async parents are told, leaf to root), which was the one M4-deferred test262 failure (`module-code/top-level-await/rejection-order.js`); its exclusion line is gone. The M4 directories (module-code, dynamic-import, import, async functions and generators, generators, for-await-of, await, Promise, Generator/AsyncGenerator/AsyncFromSyncIterator prototypes, Iterator) run 5334/5334; the full suite is 34703/34703 with the M3 exclusions (every excluded test needs an M5 built-in, listed per test in `exclusions/gable.txt`).

AC #3: added `Engine::release_module(realm, specifier) -> Result<bool>` to the engine interface (ADR 0006 addendum). The Gable engine drops the module-map entry so the next import under that specifier evaluates the module again and the old record is collected once unreachable; the QuickJS-NG adapter returns `false` because QuickJS cannot drop a loaded module. The conformance test `modules_reload_under_a_new_specifier_and_old_records_are_collected` now observes collection through a host-class finaliser (a marker object reachable only from the old module's exports) instead of `WeakRef`, so it runs on both adapters today; it asserts the re-evaluation and the finaliser on Gable and the keep-old-record behaviour on QuickJS-NG. Kit results: Gable 19/21 (the two remaining failures need ArrayBuffer and Date, M5), QuickJS-NG 22/22.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
M4 delivered: AsyncModuleExecutionRejected now rejects leaf to root (the last M4 test262 failure), Engine::release_module added to the interface so a module can be re-loaded under a specifier and its old record collected (Gable implements it, QuickJS-NG reports false), and the conformance test observes collection via a host-class finaliser on both adapters. M4 directories 5334/5334; full suite 34703/34703 with M5-only exclusions.
<!-- SECTION:FINAL_SUMMARY:END -->
