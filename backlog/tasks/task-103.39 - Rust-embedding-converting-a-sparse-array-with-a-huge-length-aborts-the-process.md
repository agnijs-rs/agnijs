---
id: TASK-103.39
title: >-
  Rust embedding: converting a sparse array with a huge length aborts the
  process
status: Done
assignee:
  - '@claude'
created_date: '2026-11-13T22:48:00Z'
updated_date: '2026-11-14T01:12:00Z'
labels:
  - bug
  - security
dependencies: []
parent_task_id: TASK-103
priority: medium
ordinal: 304000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the embedder-surface review of 2026-11 (docs/surface-review-2026-11.md), at commit fea680f.

Severity: medium. Untrusted script can abort the host process, which the memory limit does not prevent, through every Rust-embedding API that turns a JS value into Rust data.

Affected code: crates/agnijs/src/convert.rs, `walk` (about lines 85 to 93), used by `js_to_json` and `from_value`. Callers include `Context::eval::<T>`, `from_js`, `get_global`, `call`, and the argument decoding of every `register_fn` / `register_try_fn` / `register_async_fn` host function and `HostClass` method.

    let len = engine.get(realm, value, &"length".into())?;
    let len = len.as_number().unwrap_or(0.0) as u32;
    let mut items = Vec::with_capacity(len as usize);

A sparse array costs the engine almost nothing, but `length` reaches 2^32-1. Preallocating `length` slots of `serde_json::Value` (32 bytes each) asks for 137 GB before a single element is read.

Minimal repro:

    let mut ctx = agnijs::Context::new();
    ctx.register_fn("f", |_v: serde_json::Value| 0).unwrap();
    ctx.eval_value("var a = []; a.length = 4294967295; try { f(a) } catch (e) {}");
    // or: ctx.eval::<serde_json::Value>("var a = []; a.length = 4294967295; a");

Command: an integration test in crates/agnijs/tests, `cargo test -p agnijs --test <name>` (Windows, debug build).

Expected: a conversion error (`Error::Convert`, or a TypeError thrown into the script for a host-function argument). Or the conversion is bounded by the engine's memory limit.
Actual: `memory allocation of 137438953440 bytes failed`, and the process aborts (exit code 0xc0000409, STATUS_STACK_BUFFER_OVERRUN, the Windows abort status, on a clean debug build). `catch_unwind` cannot catch this, and the memory limit is never consulted. On a system that overcommits and grants the reservation, the loop that follows makes 2^32 `engine.get` calls from Rust with no interrupt or fuel check: an unbounded hang.

Root cause: a script-controlled length is trusted for a preallocation, and the element loop is a native loop with no limit.

Suggested fix: do not preallocate from `length`. Cap it with a sane conversion limit (refuse arrays longer than a fixed maximum, or longer than the remaining memory budget allows), and bound the total number of converted elements. Check the engine's interrupt or limit inside the loop, or document the cap. `Vec::with_capacity(len.min(SMALL))` alone fixes the abort but not the hang.

Test that should catch it: an embed test that passes `Object.assign([], { length: 2**32 - 1 })` to a registered function and to `eval::<serde_json::Value>`, and expects an error within a time bound.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Passing an array with length 2^32-1 to a registered host function gives a catchable error, not an abort
- [x] #2 eval::<serde_json::Value> of such an array returns Error::Convert without allocating per the length
- [x] #3 Conversion of a long array is bounded by a documented cap or the memory limit, and does not run unpolled for 2^32 steps
- [x] #4 crates/agnijs/tests has a regression test for both paths
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red test in crates/agnijs/tests: eval::<serde_json::Value> and a host-function argument with a sparse length 2^32-1 and a nested 2^23.
2. A per-conversion budget (MAX_CONVERTED_VALUES = 2^22 elements and members over the whole tree), charged before each allocation; Error::Convert past it.
3. Check the CLI's own conversions.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Red: crates/agnijs/tests/long_arrays.rs aborted with 0xc0000409 before the fix.

Fix (9203079): js_to_json carries a budget of MAX_CONVERTED_VALUES (2^22) array elements and object members over the whole value, charged before each array or object is allocated. Past it the result is Error::Convert, a TypeError in the script for host-function arguments. agnijs::MAX_CONVERTED_VALUES is exported and documented.

Decision: a cap rather than an interrupt poll, because the Engine trait has no poll; at most 2^22 steps per conversion.

CLI: the setTimeout argument Vec no longer sizes from the rest array length (dense today, hardening only).

Gates: test262 0 regressions in default, --precompile, --snapshot, --only-intl402, the QuickJS-NG baseline and the GC-stress slice; JIT forced and tiered slices (CI filters plus apply, Reflect, TypedArray copyWithin, ArrayBuffer, Atomics, BigInt, RegExp replace/split, Proxy ownKeys) 0 regressions; cargo test -p agnijs-js-vm --features jit green. Bench suite JIT off vs 226e6d1, 8 alternating rounds: geomean 1.005x.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Rust-API conversion (from_value, eval::<T>, host-function arguments) now has a documented budget of 2^22 array elements and object members per value, so a sparse array with length 2^32-1, alone or nested, gives Error::Convert (TypeError for host functions) instead of a 128 GiB preallocation and abort. long_arrays.rs covers eval and a host function, plus a 100,000-element array still converting. Commit 9203079.
<!-- SECTION:FINAL_SUMMARY:END -->
