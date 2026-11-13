---
id: TASK-103.43
title: >-
  Atomics operations panic when an index or value conversion shrinks a
  length-tracking view
status: To Do
assignee: []
created_date: '2026-11-13T22:48:00Z'
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
- [ ] #1 Every Atomics read-modify-write, load and store throws RangeError when a conversion leaves the index out of bounds
- [ ] #2 Out-of-range access in raw_read and raw_write is an error, not a panic
- [ ] #3 A VM test covers each operation, including the BigInt64 variants
<!-- AC:END -->
