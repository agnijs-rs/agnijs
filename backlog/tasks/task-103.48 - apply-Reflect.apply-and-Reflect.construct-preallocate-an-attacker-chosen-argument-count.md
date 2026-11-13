---
id: TASK-103.48
title: >-
  apply, Reflect.apply and Reflect.construct preallocate an attacker-chosen
  argument count
status: To Do
assignee: []
created_date: '2026-11-13T22:49:00Z'
labels:
  - bug
  - security
dependencies: []
parent_task_id: TASK-103
priority: medium
ordinal: 313000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the embedder-surface review of 2026-11 (docs/surface-review-2026-11.md), at commit fea680f.

Severity: medium. Script aborts the host process (`memory allocation of N bytes failed`, 0xC0000409), with or without a memory limit. This is not a panic, so no guard can catch it.

Affected code: crates/agnijs-js-vm/src/ops.rs around line 1146 (CreateListFromArrayLike). It calls `Vec::with_capacity(len)` with an array-like's `length`, up to 2^53-1. Reached from `Function.prototype.apply`, `Reflect.apply` and `Reflect.construct`. The fill loop after it does not poll the interrupt either.

Minimal repro:

    Math.max.apply(null, { length: 2 ** 40 })

Command: `agnijs eval "Math.max.apply(null, { length: 2 ** 40 })"` (debug build at fea680f).

Expected: a catchable RangeError. V8 refuses argument lists of more than about 65k to 120k, and SpiderMonkey and JSC have similar caps. Or the engine's memory limit fails the call.
Actual: the process aborts on a reservation of 2^40 values.

Root cause: a script-controlled length is trusted for a preallocation, and there is no maximum argument count.

Suggested fix: refuse argument lists above a fixed maximum (for example 2^16 to 2^20) with a RangeError before allocating. Poll the interrupt and the memory limit in the fill loop.

Test that should catch it: VM tests for `apply`, `Reflect.apply` and `Reflect.construct` with `{ length: 2**32 - 1 }` and `{ length: 2**40 }` that expect a RangeError.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 apply with an array-like of length 2^32-1 or 2^40 throws a catchable RangeError without allocating per the length
- [ ] #2 The argument list fill polls the interrupt and the memory limit
- [ ] #3 VM tests cover apply, Reflect.apply and Reflect.construct
<!-- AC:END -->
