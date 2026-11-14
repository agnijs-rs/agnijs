---
id: TASK-103.47
title: >-
  IsArray, IsConstructor and GetFunctionRealm recurse through proxy and
  bound-function chains without a stack check
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
ordinal: 312000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the embedder-surface review of 2026-11 (docs/surface-review-2026-11.md), at commit fea680f.

Severity: medium. Script aborts the host with a native stack overflow (0xC00000FD), not a RangeError. These are three call sites with the same root cause and the same fix, so they are filed as one task.

Affected code (crates/agnijs-js-vm/src):
1. `IsArray` through proxy chains: ops.rs around line 1233. Reached from `Array.isArray`, `concat`, `JSON.stringify`, `Object.prototype.toString`, ArraySpeciesCreate, and the CLI's `console.log`.
2. `is_constructor` through bound-function chains: interp/calls.rs around lines 26 to 31. It runs before the depth-checked recursion in `construct_value`.
3. `function_realm` (GetFunctionRealm) through bound and proxy chains: interp/calls.rs around lines 38 to 50.

Minimal repros:

    let p = []; for (let i = 0; i < 1e6; i++) p = new Proxy(p, {}); Array.isArray(p);
    let f = function () {}; for (let i = 0; i < 1e6; i++) f = f.bind(); new f();
    let g = function () {}; g.prototype = null; for (let i = 0; i < 1e6; i++) g = g.bind(); Reflect.construct(Array, [], g);

Command: `agnijs eval "<one line>"` (debug build at fea680f).

Expected: a catchable RangeError (the engine's call-depth and native-depth limits already turn deep proxy get, call and construct chains into one), or the correct answer computed without recursion.
Actual: exit -1073741571, `thread 'main' has overflowed its stack`. The CLI's `console.log(p)` crashed at a depth of 1e5.

Caveat: each is a self tail call, so a release build may turn some of them into loops. Confirmed on debug only.

Suggested fix: walk the chain with a loop: for a proxy, `IsArray` moves to the target and throws TypeError on a revoked proxy; `is_constructor` and `function_realm` move to the bound target or proxy target. Or call the VM's stack check at each level.

Test that should catch it: VM tests with chains of 1e6 proxies and bound functions for each of the three operations, on a default-size thread, that expect a correct result or a catchable RangeError.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Array.isArray, new and Reflect.construct over 1e6-link proxy or bound chains give a correct result or a catchable RangeError
- [x] #2 The fix holds in debug and release builds
- [x] #3 VM tests cover the three operations
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red tests in child processes: a million-deep proxy chain through Array.isArray, toString and concat; bound-function chains through new, Reflect.construct and instanceof.
2. Turn IsArray, IsConstructor and GetFunctionRealm into loops.
3. Sweep other spec operations that follow proxy targets or bound targets recursively.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Fixed in 771fff6. is_array, is_constructor and function_realm follow proxy and bound-function chains in a loop; the common case stays an inline match and the loops are out of line. Same class found and fixed: InstanceofOperator over bound functions without @@hasInstance recursed per link. That recursion is spec-shaped (it looks up @@hasInstance at each link), so it stays and now calls check_stack. Tests: vm/tests/deep_native_chains.rs, 8 cases in child processes (Array.isArray, toString and concat over proxies; new, Reflect.construct and instanceof over bound chains, with and without prototypes; proxied and bound new.target); red before the fix, green in debug and release.

Gates on the rebased tree (base 32efdce): workspace tests pass; test262 0 regressions in quickjs, parse, agnijs default, --precompile, --snapshot, --only-intl402; GC-stress slice and GC-stress unit tests pass; JIT feature VM tests and AGNIJS_JIT=force/on slices 0 regressions; C API fault-injection, fuzz (10x) and cbindgen header drift clean; clippy -D warnings clean on capi, cli, resolve and debugger, and no clippy finding on the touched lines in parser and vm (their pre-existing findings are elsewhere); benchmark with JIT off against 8af8d4a, 4 interleaved rounds of 10 iterations: geomean 0.985, compile-source 0.971, json-stringify 0.981, json-parse 0.983.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Array.isArray, new, Reflect.construct, Object.prototype.toString and instanceof over chains of a million proxies or bound functions return the right result or a catchable RangeError, in debug and release, instead of overflowing the native stack. Commit 771fff6.
<!-- SECTION:FINAL_SUMMARY:END -->
