---
id: TASK-103.27
title: >-
  Rust embedding: deeply nested input overflows the native stack without a stack
  budget
status: Done
assignee:
  - '@claude'
created_date: '2026-11-12T06:25:00Z'
updated_date: '2026-11-12T09:45:00Z'
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
- [x] #1 Engines created through the Rust API (Context::new, Context::with_host, make_engine, Vm::new) refuse deeply nested source, JSON and other recursive input with a SyntaxError or RangeError instead of overflowing, on a thread with the default 2 MiB Rust stack, by default
- [x] #2 A Rust embedder can set the budget (or opt out) through a documented API
- [x] #3 Regression tests run deeply nested array literals, nested functions and JSON.parse on a 2 MiB thread and on a smaller one
- [x] #4 The CLI gets the same protection
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Failing tests first: crates/agnijs-js-vm/tests/stack_budget.rs (Vm::new and make_engine) and crates/agnijs/tests/stack_budget.rs (Context::new, Context::with_host): 300,000-deep array literal, nested functions, JSON.parse of nested text and runtime recursion, each on a 2 MiB thread and on a 1 MiB one (the Windows main-thread default), expecting SyntaxError / RangeError and a usable engine afterwards.
2. Engine::set_stack_budget(Option<usize>) and Engine::stack_budget() on the engine-api trait (defaults: Unsupported / None). DEFAULT_BUDGET (512 KiB) and HEADROOM (256 KiB) constants in agnijs_js_parser::stack, checked equal to the C API's.
3. Vm keeps stack_budget: Option<usize>, Some(512 KiB) from Vm::bare, so Vm::new, make_engine, snapshot restores, debuggees and Context all get it. Every Engine entry that compiles or runs code installs StackBudget::enter (Vm::enter plus the compile paths); nested entries keep the outer, tighter limit. None opts out (an embedder's own outer StackBudget still applies).
4. C API: also sets the engine's budget to its stack_budget option, so a larger C budget is not tightened by the engine default.
5. CLI: protected through Context; add cli.rs tests that deep source and JSON.parse exit with an error, not a crash.
6. Update docs/security-model.md (native stack section, limits intro, checklist, example) and the Engine docs; run the gates; commit; push.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Red: the new crates/agnijs-js-vm/tests/stack_budget.rs died with STATUS_STACK_OVERFLOW before the change. Fix: Vm::stack_budget (default agnijs_js_parser::stack::DEFAULT_BUDGET, 512 KiB) installed by Vm::enter and the compile paths (eval_script, compile_script, compile_module, eval_bytecode); Engine::set_stack_budget / stack_budget (None opts out); the C API passes its stack_budget option to the engine; Inspected (CLI --inspect) forwards both. Also found and fixed a parser gap: nested function declarations went through parse_statement_list_item, which had no budget check, and overflowed a 1 MiB thread even under a budget (the C API sweep never tried them).
Sizing: kept 512 KiB (same as the C API), so 1 MiB threads (Windows main thread, the CLI) stay protected. Measured deepest nesting the default accepts (Windows x64): release array 160, object 133, function 185, regex groups 256 (the limit), JSON.parse 1202; debug array 18, object 14, function 50, regex 116, JSON 263. The regexp test that runs patterns at the 256-group limit on a 2 MiB thread now sets a 1.5 MiB budget (debug builds need it). test262 runner keeps the default: 0 regressions.

Gates (worktree at b3f3226 + this change, Windows x64): cargo test --workspace --locked --no-fail-fast all ok; test262 default 43192/43192 passed, --precompile 43192, --snapshot 43192, --only-intl402 981, 0 regressions each; QuickJS-NG baseline 0 regressions (43157 tests, 84 baseline failures); C API with fault-injection ok (56 tests), release fuzz ok (5), clippy -D warnings clean for agnijs-capi and agnijs-cli, no new clippy warnings in touched files of the other crates; cbindgen header: no drift; GC-stress test262 slice (AGNIJS_GC_STRESS=16) 5959/5959, 0 regressions; AGNIJS_GC_STRESS=1 on the new vm stack_budget and regexp tests and the CI capi stress set: ok. rustfmt applied to my hunks only (the files are not rustfmt-clean elsewhere).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Every engine now installs a native stack budget by default, so deeply nested source, JSON and other recursive input fail with a SyntaxError or RangeError (Maximum call stack size exceeded) instead of overflowing the thread and killing the process. Vm keeps stack_budget (default 512 KiB, agnijs_js_parser::stack::DEFAULT_BUDGET, plus 256 KiB HEADROOM: fits 2 MiB and 1 MiB threads) and installs StackBudget::enter in Vm::enter and the compile paths; nested entries keep the outer, tighter limit. New Engine::set_stack_budget(Option<usize>) / stack_budget() (None opts out; QuickJS returns Unsupported); Context reaches them via engine(); the C API forwards its own stack_budget option, with compile-time checks that its constants equal the engine's; the CLI is covered through Context and Inspected forwards the methods. Fixed a parser gap: nested function declarations skipped the budget check (parse_statement_list_item). Tests: crates/agnijs-js-vm/tests/stack_budget.rs and crates/agnijs/tests/stack_budget.rs (2 MiB and 1 MiB threads; arrays, functions, parens, arrows, JSON.parse, eval, Function, RegExp, JSON.stringify, flat, Proxy; default, change, opt-out, reentry) and a CLI test. Docs: security-model.md native stack section, limits intro, checklist, example; ADR 0021 addendum. Risk: debug builds use ~10x the stack per level, so the default accepts only shallow nesting there (arrays 18, functions 50; release 160 and 185); embedders can raise the budget on a larger thread.
<!-- SECTION:FINAL_SUMMARY:END -->
