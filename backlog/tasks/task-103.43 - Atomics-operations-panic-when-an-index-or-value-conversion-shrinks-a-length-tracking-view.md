---
id: TASK-103.43
title: >-
  Atomics operations panic when an index or value conversion shrinks a
  length-tracking view
status: Done
assignee:
  - '@claude'
created_date: '2026-11-13T22:48:00Z'
updated_date: '2026-11-14T01:12:00Z'
labels:
  - bug
dependencies: []
parent_task_id: TASK-103
priority: medium
ordinal: 308000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the embedder-surface review of 2026-11 (docs/surface-review-2026-11.md), at commit fea680f.

Severity: medium (a panic reachable from any script).

Affected code: crates/agnijs-js-vm/src/builtins/atomics.rs: `validate_atomic_access` (about line 64), `revalidate` (about line 83), `raw_read` (line 99) and `raw_write` (line 105). Affected operations: add, sub, and, or, xor, exchange, compareExchange, load and store, including the BigInt64 variants.

Minimal repro (p3_atomics.js; p4_atomics_load.js is the same with the index):

    var rab = new ArrayBuffer(16, { maxByteLength: 16 });
    var ta = new Int32Array(rab);
    try {
      console.log(Atomics.add(ta, 3, { valueOf() { rab.resize(4); return 1; } }));
    } catch (e) { console.log("caught", e.name, e.message); }

    // p4: Atomics.load(ta, { valueOf() { rab.resize(4); return 3; } })

Command: `agnijs run p3_atomics.js`.

Expected: "caught RangeError ...". The spec's RevalidateAtomicAccess throws a RangeError when the index is no longer below the current length.
Actual: exit 101, `panicked at crates\agnijs-js-vm\src\builtins\atomics.rs:99:26: range start index 12 out of range for slice of length 4`. The store and exchange paths panic at atomics.rs:105:35.

Root cause: the index is checked against the length captured before the index or value `valueOf` runs. `revalidate` checks only that the view is not detached or out of bounds; it does not re-check `index < length`. Then `raw_read` and `raw_write` slice out of range.

Suggested fix: in `revalidate`, re-read the typed array's length and throw RangeError when `index >= len`. Also check the result in `raw_read` and `raw_write` with `get`, so a missed path is an error, not a panic.

Test that should catch it: a VM test that shrinks a length-tracking view from the index valueOf and from the value valueOf, for each Atomics operation, and expects a RangeError.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Every Atomics read-modify-write, load and store throws RangeError when a conversion leaves the index out of bounds
- [x] #2 Out-of-range access in raw_read and raw_write is an error, not a panic
- [x] #3 A VM test covers each operation, including the BigInt64 variants
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red test: every RMW op, load, store and compareExchange, shrinking at each conversion, Int32 and BigInt64.
2. RevalidateAtomicAccess after conversions: RangeError when the element is past the buffer's current byte length.
3. raw_read and raw_write return errors on out-of-range slices instead of panicking.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Red: resizable_reentry.rs panicked at atomics.rs:99 before the fix.

Fix (977e84d): revalidate checks the element against the buffer current byte length after the conversions and throws RangeError (RevalidateAtomicAccess). raw_read and raw_write return JsResult and use get/get_mut, so an out-of-range slice is an error, not a panic.

wait/notify: shared buffers cannot shrink, and wait on a non-shared buffer throws before any access.

Micro-benchmark: Int32 Atomics 1.085x (the spec-required length check), BigInt64 Atomics 0.73x.

Gates: test262 0 regressions in default, --precompile, --snapshot, --only-intl402, the QuickJS-NG baseline and the GC-stress slice; JIT forced and tiered slices (CI filters plus apply, Reflect, TypedArray copyWithin, ArrayBuffer, Atomics, BigInt, RegExp replace/split, Proxy ownKeys) 0 regressions; cargo test -p agnijs-js-vm --features jit green. Bench suite JIT off vs 226e6d1, 8 alternating rounds: geomean 1.005x.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Every Atomics read-modify-write, compareExchange, load and store re-validates the index after converting its arguments and throws RangeError when the buffer shrank under it; the raw byte accessors return errors instead of panicking. Tests run each operation with a shrink at each conversion on Int32 and BigInt64 views. Commit 977e84d.
<!-- SECTION:FINAL_SUMMARY:END -->
