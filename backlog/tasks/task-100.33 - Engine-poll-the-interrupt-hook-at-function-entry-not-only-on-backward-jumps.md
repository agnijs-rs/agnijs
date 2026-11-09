---
id: TASK-100.33
title: 'Engine: poll the interrupt hook at function entry, not only on backward jumps'
status: To Do
assignee: []
created_date: '2026-11-09T05:53:00Z'
labels:
  - needs-triage
dependencies: []
documentation:
  - docs/adr/0019-jit-readiness-constraints-tiering-and-deoptimization.md
parent_task_id: TASK-100
ordinal: 282000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Vm::poll_interrupt is called only from the jump macro in interp/step.rs when the offset is not positive, that is on loop back-edges. Code that recurses without looping, such as a naive fib(40), never polls, so the host interrupt hook (and with it any time limit an embedder sets) cannot stop it until the frame limit is hit. ADR 0019 (J7) asks every tier to poll one attention word at function entry, loop back-edges and after calls return; the interpreter should already do so at entry. TASK-100.17 replaces the per-instruction flag tests with an attention word, so this can land there or before it. Found while checking the engine against ADR 0019 (TASK-101.1).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A test shows a recursive function with no loops is interrupted by the host interrupt hook
- [ ] #2 The entry poll costs no more than 2% on calls-recursive, measured before and after
- [ ] #3 test262 passes with no new failures or exclusions in both the source and --precompile runs
<!-- AC:END -->
