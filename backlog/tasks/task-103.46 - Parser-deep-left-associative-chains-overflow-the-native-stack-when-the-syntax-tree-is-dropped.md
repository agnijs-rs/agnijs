---
id: TASK-103.46
title: >-
  Parser: deep left-associative chains overflow the native stack when the syntax
  tree is dropped
status: Done
assignee:
  - '@claude'
created_date: '2026-11-13T22:48:00Z'
updated_date: '2026-11-14T06:25:00Z'
labels:
  - bug
  - security
dependencies: []
parent_task_id: TASK-103
priority: medium
ordinal: 311000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the embedder-surface review of 2026-11 (docs/surface-review-2026-11.md), at commit fea680f.

Severity: medium. Source text aborts the host process with a native stack overflow (0xC00000FD), not a RangeError or SyntaxError. The stack budget (TASK-103.27) cannot stop it.

Affected code: crates/agnijs-js-parser. The parser builds left-associative chains in a loop without `check_stack`: binary and logical operators (`+`, `||`, `??`, ...) in parser/expr.rs around lines 366 and 391, and member, call, optional-chain, `new` and tagged-template chains around lines 598 and 610. The loop itself is safe, but the resulting tree is as deep as the chain. The compiler refuses it correctly (about 2,000 levels in debug). The AST has no iterative `Drop`, so dropping the tree recurses once per level and overflows. It also crashes when the parse fails at the end of the chain, which shows that the drop is the cause.

Minimal repro (either one):

    eval('1' + '+1'.repeat(1e6))

    // or a file of about 400 KB: `var x = Object` followed by `?.p` repeated 200,000 times

Command: `agnijs run <file>` (debug build at fea680f), or `agnijs eval "eval('1' + '+1'.repeat(1e6))"`.

Expected: a catchable RangeError or SyntaxError ("too deeply nested"), as for parentheses, ternaries and the other nesting forms the review tried (about 60 constructs).
Actual: exit -1073741571, `thread 'main' has overflowed its stack`. Debug crashes from about 5,000 levels. Release is unconfirmed but expected to crash at these depths, because the drop recursion is not a tail call.

Suggested fix: bound the chain depth in those parser loops with `check_stack` or a depth counter, so the tree is never deeper than the compiler accepts. Alternatively, give the expression tree an iterative `Drop`, which also protects embedders that build ASTs themselves.

Test that should catch it: parser and VM tests for each chain form (binary, logical, `??`, `.x`, `?.x`, call, `new`, tagged template) at 1e6 links, at top level and inside eval, that expect a catchable error. Run them on a thread with the default stack.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Binary, logical, ??, member, optional-chain, call, new and tagged-template chains of 1e6 links give a catchable error at top level and in eval
- [x] #2 No AST drop recurses deeper than the compiler accepts
- [x] #3 Parser or VM tests cover each chain form on a default-size thread
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red tests that run each deep-chain form in a child process (a stack overflow fails one test, not the suite): parser parse+drop and VM top-level and eval.
2. Drop impls for Binary, Logical, Member, Call, New and TaggedTemplate that detach the left spine and drop it in a loop.
3. Check the compiler and other AST walkers stop at the budget.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Fixed in b5f79bb. BinaryExpression, LogicalExpression, MemberExpression, CallExpression, NewExpression and TaggedTemplate implement Drop: when the left spine below a node is more than one chain link deep, the node detaches it and drops the links in a loop, so no AST drop recurses along a chain; short chains pay one check. The compiler already stops at the stack budget, so a 1e6-link chain is a catchable RangeError/SyntaxError. parser/expr.rs uses mem::take for call.arguments (Drop forbids the move). Sweep: no other unchecked recursive AST walkers (compiler and analysis check the stack). Tests run each form in a child process: parser/tests/deep_chains.rs (14 forms, script and module, default-size thread) and vm/tests/deep_chains.rs (11 forms, top level and eval); red before the fix, green in debug and release.

Gates on the rebased tree (base 32efdce): workspace tests pass; test262 0 regressions in quickjs, parse, agnijs default, --precompile, --snapshot, --only-intl402; GC-stress slice and GC-stress unit tests pass; JIT feature VM tests and AGNIJS_JIT=force/on slices 0 regressions; C API fault-injection, fuzz (10x) and cbindgen header drift clean; clippy -D warnings clean on capi, cli, resolve and debugger, and no clippy finding on the touched lines in parser and vm (their pre-existing findings are elsewhere); benchmark with JIT off against 8af8d4a, 4 interleaved rounds of 10 iterations: geomean 0.985, compile-source 0.971, json-stringify 0.981, json-parse 0.983.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Parsing and then dropping a left-associative chain of a million links (binary, logical, ??, member, optional, call, new, tagged template) no longer overflows the native stack. The AST drops such chains iteratively, and the VM reports a catchable error at top level and in eval. Commit b5f79bb.
<!-- SECTION:FINAL_SUMMARY:END -->
