---
id: TASK-101.3
title: Baseline JIT behind a feature flag
status: To Do
assignee: []
created_date: '2026-11-09T01:52:00Z'
updated_date: '2026-11-09T05:53:00Z'
labels:
  - needs-triage
dependencies:
  - TASK-101.1
  - TASK-101.2
  - TASK-100.12
parent_task_id: TASK-101
ordinal: 231000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
An optional baseline (template) JIT behind a cargo feature or separate crate: compile hot bytecode to machine code without optimization, using inline-cache feedback, with deoptimization back to the interpreter so breakpoints, stepping, hot update and the memory and interrupt limits keep working. Choose the code generator (Cranelift or a hand-rolled assembler) in the ADR from 101.1 before starting. Unsafe confined to one small crate. The interpreter stays the default and the portable fallback.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Hot functions run as machine code on the supported architectures with a measured speedup on gable-js-bench workloads, and the interpreter fallback is selected on every unsupported target
- [ ] #2 test262 passes at the baselines in a JIT-forced mode and a differential fuzz test against the interpreter runs in CI; debugger and interrupt tests pass with the JIT enabled
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
From TASK-101.1 (ADR 0019): code generator decided as Cranelift (one IR for x86-64 and AArch64, verifier, cranelift-object for AOT in TASK-101.5); measure compile time per function first and reopen the choice if it exceeds about a fifth of the gain on the Gable set. The baseline keeps the interpreter frame layout (Sparkplug-style), so deopt is continue-at-pc and OSR a jump. Prerequisites: defined Value and object-header layout (J5), one JS entry point (J8, TASK-100.19), attention word (J7, TASK-100.17). Cargo feature jit on agnijs-js-vm, crate agnijs-js-jit (only engine crate with unsafe), runtime executable-memory probe with interpreter fallback; never compiled for iOS or wasm32.
<!-- SECTION:NOTES:END -->
