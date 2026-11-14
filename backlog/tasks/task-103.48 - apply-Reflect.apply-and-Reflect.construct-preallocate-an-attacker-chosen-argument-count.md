---
id: TASK-103.48
title: >-
  apply, Reflect.apply and Reflect.construct preallocate an attacker-chosen
  argument count
status: Done
assignee:
  - '@claude'
created_date: '2026-11-13T22:49:00Z'
updated_date: '2026-11-14T01:12:00Z'
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
- [x] #1 apply with an array-like of length 2^32-1 or 2^40 throws a catchable RangeError without allocating per the length
- [x] #2 The argument list fill polls the interrupt and the memory limit
- [x] #3 VM tests cover apply, Reflect.apply and Reflect.construct
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red test: apply, Function.prototype.apply.call, Reflect.apply, Reflect.construct, proxy ownKeys with lengths 2^32-1, 2^40, 2^53-1; lowered memory limit; interrupt.
2. Vm::check_list_length: RangeError past MAX_LIST_LENGTH (2^22), then check_memory for the list; poll the interrupt in the fill loop.
3. Sweep other with_capacity sites sized by script input.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Red: untrusted_lengths.rs aborted (allocation failure) or timed out before the fix.

Fix (74a0c56): Vm::check_list_length throws RangeError past MAX_LIST_LENGTH (2^22, ops.rs), then check_memory for the list, before it is allocated. create_list_from_array_like polls the interrupt while it fills the list. This covers apply, Reflect.apply, Reflect.construct and proxy ownKeys.

Decision: a dense array passed to apply takes the existing fast path, which copies elements that already exist, so it is not capped.

Perf (1100ba9): with the checks added, the function was inlined into the interpreter call path and closures ran 7.6% slower; #[inline(never)] brought it back to 0.99x.

Sibling sweep, no bug found: array_to_vec, iterator zip, Date arguments, the C API host argc, async cancel_pending. The CLI setTimeout argument Vec was sized by the rest array length; it is dense, but now capped at 64 (9203079).

docs/security-model.md has a new section on lists sized by a script length.

Gates: test262 0 regressions in default, --precompile, --snapshot, --only-intl402, the QuickJS-NG baseline and the GC-stress slice; JIT forced and tiered slices (CI filters plus apply, Reflect, TypedArray copyWithin, ArrayBuffer, Atomics, BigInt, RegExp replace/split, Proxy ownKeys) 0 regressions; cargo test -p agnijs-js-vm --features jit green. Bench suite JIT off vs 226e6d1, 8 alternating rounds: geomean 1.005x.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
CreateListFromArrayLike refuses more than 2^22 values with a catchable RangeError, checks the memory limit for the list before allocating it and polls the interrupt while filling it, so apply, Reflect.apply, Reflect.construct and proxy ownKeys with a length of 2^32-1, 2^40 or 2^53-1 no longer abort. Tests cover each entry point, a 4 MiB memory limit and the interrupt. Commits 74a0c56 and 1100ba9 (keeps the slow path out of line; bench geomean 1.005x).
<!-- SECTION:FINAL_SUMMARY:END -->
