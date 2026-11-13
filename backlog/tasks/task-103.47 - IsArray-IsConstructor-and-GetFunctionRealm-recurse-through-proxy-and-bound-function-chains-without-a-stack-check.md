---
id: TASK-103.47
title: >-
  IsArray, IsConstructor and GetFunctionRealm recurse through proxy and
  bound-function chains without a stack check
status: To Do
assignee: []
created_date: '2026-11-13T22:48:00Z'
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
- [ ] #1 Array.isArray, new and Reflect.construct over 1e6-link proxy or bound chains give a correct result or a catchable RangeError
- [ ] #2 The fix holds in debug and release builds
- [ ] #3 VM tests cover the three operations
<!-- AC:END -->
