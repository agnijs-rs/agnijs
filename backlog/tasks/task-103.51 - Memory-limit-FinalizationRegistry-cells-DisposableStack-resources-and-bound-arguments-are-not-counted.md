---
id: TASK-103.51
title: >-
  Memory limit: FinalizationRegistry cells, DisposableStack resources and bound
  arguments are not counted
status: To Do
assignee: []
created_date: '2026-11-13T22:49:00Z'
labels:
  - bug
  - security
dependencies: []
parent_task_id: TASK-103
priority: medium
ordinal: 316000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the embedder-surface review of 2026-11 (docs/surface-review-2026-11.md), at commit fea680f.

Severity: medium (memory-limit bypasses). These are three classes with one root cause, filed as one task.

Affected code: crates/agnijs-js-vm/src/object.rs around line 473, `Class::payload_bytes`, returns 0 for classes whose payload grows without bound:
1. FinalizationRegistry cells (builtins/collections.rs around line 1268; `register` pushes a cell per call).
2. DisposableStack and AsyncDisposableStack resources (builtins/disposable.rs around lines 145 and 372; `defer`, `use` and `adopt` push).
3. Bound functions' argument lists (interp/calls.rs around line 742).

Minimal repros (each with `--max-memory 16M`):

    const r = new FinalizationRegistry(() => {}); const t = {}; for (;;) r.register(t, 1);
    const s = new DisposableStack(); const f = () => {}; for (;;) s.defer(f);
    const big = new Array(1e5).fill(0); const k = []; function f() {} for (;;) k.push(f.bind(null, ...big));

Command: `agnijs run --max-memory 16M <file>` (debug build at fea680f).

Expected: exit 3 (out of memory) at about the limit.
Actual: in 20 s with no out-of-memory, the FinalizationRegistry loop reaches 1.5 to 3.0 GiB and the DisposableStack loop 0.65 to 2.5 GiB (two runs each). Each bound function carries about 1.6 MB of uncounted arguments, so the third loop passes 250 to 330 MiB in 30 s and keeps growing.

Suggested fix: return the real payload size from `payload_bytes` for these classes (cells × cell size, resources × entry size, bound args × value size). Charge it when the payload grows (`register`, `defer`, `use`, `adopt`, `bind`), as Map and Set entries are charged (TASK-103.25).

Test that should catch it: memory_limit.rs tests that grow each payload in a loop under a small limit and expect OutOfMemory. Also an audit test that lists every `Class` and asserts which ones may report 0.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Growing each of the three payloads in a loop under a small limit ends in OutOfMemory near the limit
- [ ] #2 Class::payload_bytes reports a real size for every class with a growable payload, and a test lists the classes allowed to report 0
- [ ] #3 memory_limit.rs covers the three classes
<!-- AC:END -->
