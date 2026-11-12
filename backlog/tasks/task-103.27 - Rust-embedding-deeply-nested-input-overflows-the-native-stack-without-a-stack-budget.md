---
id: TASK-103.27
title: >-
  Rust embedding: deeply nested input overflows the native stack without a stack
  budget
status: To Do
assignee: []
created_date: '2026-11-12T06:25:00Z'
labels:
  - ready-for-agent
  - security
dependencies: []
references:
  - docs/security-model.md
parent_task_id: TASK-103
priority: high
ordinal: 291000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the security review (TASK-103.9, docs/security-model.md). The parser, compiler, JSON and other recursive built-ins stop deep recursion only when an embedder has installed a stack budget (`agnijs_js_parser::stack::StackBudget`, re-exported by agnijs-js-regex `stack.rs`: without a budget, `exhausted()` is always false, and `Vm::check_stack` never fails). Only the C API installs one (`engine_op` in `agnijs-capi/src/registry.rs`). The `agnijs` crate (`Context`), `agnijs_js_vm::make_engine`/`Vm::new` and the CLI do not, and the budget type is not part of the documented Rust surface. A probe on a Rust test thread (2 MiB stack) ran `eval_script` with 300,000 `[` characters, and separately `JSON.parse` of the same text: both overflowed the stack and the process exited (STATUS_STACK_OVERFLOW). The interpreter's own frame and native-depth caps (`MAX_FRAMES`, `MAX_NATIVE_DEPTH` in `vm.rs`) do not cover the recursive-descent parser and compiler. A script source an embedder takes from users can therefore kill the host process.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Engines created through the Rust API (Context::new, Context::with_host, make_engine, Vm::new) refuse deeply nested source, JSON and other recursive input with a SyntaxError or RangeError instead of overflowing, on a thread with the default 2 MiB Rust stack, by default
- [ ] #2 A Rust embedder can set the budget (or opt out) through a documented API
- [ ] #3 Regression tests run deeply nested array literals, nested functions and JSON.parse on a 2 MiB thread and on a smaller one
- [ ] #4 The CLI gets the same protection
<!-- AC:END -->
