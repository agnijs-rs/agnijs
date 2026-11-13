---
id: TASK-103.41
title: >-
  TypedArray.prototype.copyWithin panics when an argument shrinks the resizable
  buffer
status: To Do
assignee: []
created_date: '2026-11-13T22:48:00Z'
labels:
  - bug
dependencies: []
parent_task_id: TASK-103
priority: medium
ordinal: 306000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the embedder-surface review of 2026-11 (docs/surface-review-2026-11.md), at commit fea680f.

Severity: medium (a panic reachable from any script; the C API poisons the engine, and the CLI exits).

Affected code: crates/agnijs-js-vm/src/builtins/typedarray_ctor.rs, `proto_copy_within` (about lines 559 to 587).

Minimal repro (p1_copywithin.js):

    var rab = new ArrayBuffer(16, { maxByteLength: 16 });
    var ta = new Uint8Array(rab);
    try {
      ta.copyWithin(0, 10, { valueOf() { rab.resize(2); return 16; } });
      console.log("ok", ta.length);
    } catch (e) { console.log("caught", e.name, e.message); }

Command: `agnijs run p1_copywithin.js`.

Expected: no panic. The spec re-reads the length after argument conversion and copies only what is still in bounds, so here nothing is copied and "ok 2" prints.
Actual: exit 101, `panicked at library\core\src\slice\index.rs:1009:51: range end index 10 out of range for slice of length 2`. Other argument mixes panic at `typedarray_ctor.rs:583:21: dest is out of bounds`.

Root cause: `to`, `from` and `final` are computed against the length before conversion. After `valueOf` shrinks the resizable buffer, the length is re-read but the byte offsets are not clamped to it, and `bytes.copy_within(...)` slices out of range.

Suggested fix: after re-reading the length, return early when `from` or `to` is at or past the new length, and clamp the count to `new_len - max(from, to)`, as in the spec's TypedArray.prototype.copyWithin step that recomputes `len`.

Test that should catch it: a VM test that shrinks the buffer from each of the three argument `valueOf`s (target, start, end) for length-tracking and fixed-length views, and expects no panic and the spec result. test262 has related cases under built-ins/TypedArray/prototype/copyWithin/coerced-*-resize; check that they run.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Shrinking the buffer from the target, start or end valueOf never panics and gives the spec result
- [ ] #2 A VM test covers length-tracking and fixed-length views
<!-- AC:END -->
