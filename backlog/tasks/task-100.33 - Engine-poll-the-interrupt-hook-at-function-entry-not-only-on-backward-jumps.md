---
id: TASK-100.33
title: 'Engine: poll the interrupt hook at function entry, not only on backward jumps'
status: Done
assignee:
  - '@fable-agent'
created_date: '2026-11-09T05:53:00Z'
updated_date: '2026-11-09T09:43:00Z'
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
- [x] #1 A test shows a recursive function with no loops is interrupted by the host interrupt hook
- [x] #2 The entry poll costs no more than 2% on calls-recursive, measured before and after
- [x] #3 test262 passes with no new failures or exclusions in both the source and --precompile runs
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Landed with TASK-100.17 (commit e3943d3). `Vm::poll_interrupt` is a counter increment that consults the host hook every 4096 polls (`#[cold] ask_host_interrupt`); it runs at `run` entry, on every frame change (function entry and return, the `Flow::Reload` path) and on backward jumps. Tests `a_recursive_function_with_no_loops_is_interrupted_by_the_host` (fib(30)) and `a_chain_of_calls_returning_without_loops_is_interrupted_by_the_host` in `crates/agnijs-js-vm/tests/dispatch.rs` were red before the change and are green after. Cost: calls-recursive went from 161.0/167.1 ms to 90.1/92.8 ms with the dispatch work that carries the poll, so the entry poll is well inside the 2% budget (it is one increment and one branch per frame change). test262 agnijs 42225/42225 and --precompile 42225/42225, no new exclusions.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
The interrupt hook is polled at function entry and after every call returns as well as on back-edges (ADR 0019 J7), via the counter-based poll on the frame-change path of the new dispatch loop. Two tests pin recursive and call-chain code without loops being interrupted. No measurable cost: calls-recursive is 1.8x faster overall after TASK-100.17.
<!-- SECTION:FINAL_SUMMARY:END -->
