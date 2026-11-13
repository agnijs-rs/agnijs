---
id: TASK-103.42
title: >-
  ArrayBuffer.prototype.slice panics when the buffer shrinks during argument
  conversion or the species constructor
status: To Do
assignee: []
created_date: '2026-11-13T22:48:00Z'
labels:
  - bug
dependencies: []
parent_task_id: TASK-103
priority: medium
ordinal: 307000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the embedder-surface review of 2026-11 (docs/surface-review-2026-11.md), at commit fea680f.

Severity: medium (a panic reachable from any script).

Affected code: crates/agnijs-js-vm/src/builtins/arraybuffer.rs, `slice_impl` (about lines 330 to 391); the slice is at line 386.

Minimal repro (p2_abslice.js):

    var rab = new ArrayBuffer(16, { maxByteLength: 16 });
    try {
      var r = rab.slice(8, { valueOf() { rab.resize(2); return 16; } });
      console.log("ok", r.byteLength);
    } catch (e) { console.log("caught", e.name, e.message); }

Command: `agnijs run p2_abslice.js`.

Expected: "ok 8". The spec copies bytes only while `first < currentLen`, so the result is 8 zero bytes.
Actual: exit 101, `panicked at crates\agnijs-js-vm\src\builtins\arraybuffer.rs:386:57: range start index 8 out of range for slice of length 2`. A shrink inside the start argument's valueOf, or inside a species constructor, panics the same way.

Root cause: `len` is captured before user code runs (argument conversion, species constructor). Then `count = new_len.min(cur_len.saturating_sub(first))` and `bytes[first..first + count]` run with `first > cur_len`. The `saturating_sub` makes the count 0, but the start index is still out of range.

Suggested fix: copy only when `first < cur_len`, per the spec's ArrayBuffer.prototype.slice steps that re-read the current length.

Test that should catch it: a VM test that shrinks a resizable buffer from the start valueOf, from the end valueOf and from a species constructor, and expects no panic and a zero-filled result of the requested length.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Shrinking the buffer from either argument or from a species constructor never panics and gives the spec result
- [ ] #2 A VM test covers all three shrink points
<!-- AC:END -->
