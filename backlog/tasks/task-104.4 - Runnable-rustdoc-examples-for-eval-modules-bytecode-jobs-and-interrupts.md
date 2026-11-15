---
id: TASK-104.4
title: Runnable rustdoc examples for eval, modules, bytecode, jobs and interrupts
status: To Do
assignee:
  - '@claude'
created_date: '2026-11-15T22:22:00Z'
labels: []
dependencies: []
parent_task_id: TASK-104
type: docs
ordinal: 327000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Embedders read rustdoc first. The engine interface methods have prose but no runnable examples; the only examples are the Context-level cookbook, which does not cover modules, bytecode or the interrupt hook.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 eval_script, load_module (with the Host resolve_module and load_module hooks), compile_script/compile_module/eval_bytecode, run_jobs and Host::interrupt each have a runnable rustdoc example, run by cargo test
- [ ] #2 The examples show error and exception handling: a thrown exception with name, message and stack, a syntax error, a module load failure, a bytecode mismatch, and an interrupted run
<!-- AC:END -->
