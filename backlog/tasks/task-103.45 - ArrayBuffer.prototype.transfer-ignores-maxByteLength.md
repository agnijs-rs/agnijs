---
id: TASK-103.45
title: ArrayBuffer.prototype.transfer ignores maxByteLength
status: To Do
assignee: []
created_date: '2026-11-13T22:48:00Z'
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
- [ ] #1 transfer(n) on a resizable buffer with n above maxByteLength throws RangeError and leaves the source attached
- [ ] #2 A VM test covers transfer and transferToFixedLength
<!-- AC:END -->
