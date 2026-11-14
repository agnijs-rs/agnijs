---
id: TASK-103.45
title: ArrayBuffer.prototype.transfer ignores maxByteLength
status: Done
assignee:
  - '@claude'
created_date: '2026-11-13T22:48:00Z'
updated_date: '2026-11-14T01:12:00Z'
labels:
  - bug
dependencies: []
parent_task_id: TASK-103
priority: low
ordinal: 310000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the embedder-surface review of 2026-11 (docs/surface-review-2026-11.md), at commit fea680f.

Severity: low. A spec bug that breaks a buffer invariant (byteLength above maxByteLength). No crash was found from it, but later resize and view-length code assumes `byteLength <= maxByteLength`.

Affected code: crates/agnijs-js-vm/src/builtins/arraybuffer.rs, `transfer_impl` (about line 413), which calls `allocate_buffer(vm, proto, new_len, max, false)` without checking `new_len` against `max`.

Minimal repro (p5_transfer.js):

    var rab = new ArrayBuffer(4, { maxByteLength: 8 });
    try {
      var t = rab.transfer(100);
      console.log("no error: byteLength", t.byteLength, "max", t.maxByteLength, "resizable", t.resizable);
      var ta = new Uint8Array(t);
      console.log("ta.length", ta.length);
      t.resize(8);
      console.log("after resize", t.byteLength, ta.length);
    } catch (e) { console.log("caught", e.name, e.message); }

Command: `agnijs run p5_transfer.js`.

Expected: "caught RangeError ...". ArrayBufferCopyAndDetach throws a RangeError when the new length exceeds the max byte length of a resizable buffer. Node 24 throws `RangeError: Invalid array buffer length`.
Actual: `no error: byteLength 100 max 8 resizable true`, then `ta.length 100`, then `after resize 8 8`.

Root cause: the max byte length check is missing for the preserve-resizability path.

Suggested fix: when the source is resizable and `new_len > max`, throw a RangeError before detaching the source.

Test that should catch it: a VM test for `transfer(n)` with n above maxByteLength (expects RangeError, source not detached), and `transferToFixedLength(n)` (no max applies).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 transfer(n) on a resizable buffer with n above maxByteLength throws RangeError and leaves the source attached
- [x] #2 A VM test covers transfer and transferToFixedLength
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red test: transfer above maxByteLength, transfer up to it, transferToFixedLength.
2. AllocateArrayBuffer throws RangeError when byteLength > maxByteLength; transfer allocates before detaching, so the source stays attached.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Red: resizable_reentry.rs got byteLength 100 with maxByteLength 8 before the fix.

Fix (977e84d): AllocateArrayBuffer (allocate_buffer) throws RangeError when byteLength exceeds maxByteLength. transfer allocates the new buffer before detaching the source, so the source stays attached on the error.

Gates: test262 0 regressions in default, --precompile, --snapshot, --only-intl402, the QuickJS-NG baseline and the GC-stress slice; JIT forced and tiered slices (CI filters plus apply, Reflect, TypedArray copyWithin, ArrayBuffer, Atomics, BigInt, RegExp replace/split, Proxy ownKeys) 0 regressions; cargo test -p agnijs-js-vm --features jit green. Bench suite JIT off vs 226e6d1, 8 alternating rounds: geomean 1.005x.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
allocate_buffer now enforces byteLength <= maxByteLength as AllocateArrayBuffer requires, so transfer(n) above the maximum throws RangeError and leaves the source attached; transfer up to the maximum keeps resizability and transferToFixedLength is unaffected. Commit 977e84d.
<!-- SECTION:FINAL_SUMMARY:END -->
