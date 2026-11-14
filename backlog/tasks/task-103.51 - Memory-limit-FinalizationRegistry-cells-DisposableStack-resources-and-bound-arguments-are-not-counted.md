---
id: TASK-103.51
title: >-
  Memory limit: FinalizationRegistry cells, DisposableStack resources and bound
  arguments are not counted
status: Done
assignee:
  - '@claude'
created_date: '2026-11-13T22:49:00Z'
updated_date: '2026-11-14T10:21:00Z'
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
- [x] #1 Growing each of the three payloads in a loop under a small limit ends in OutOfMemory near the limit
- [x] #2 Class::payload_bytes reports a real size for every class with a growable payload, and a test lists the classes allowed to report 0
- [x] #3 memory_limit.rs covers the three classes
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red tests in memory_limit.rs for the three repros plus an exhaustive audit of Class.
2. payload_bytes reports every run-time-sized payload; charge growth at the push sites.
3. Audit classes beyond the three named ones.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Red: the FinalizationRegistry loop aborted the test process (allocation of 103,079,215,104 bytes failed); the audit test failed. Audit beyond the three named classes found four more payloads the program sizes: generator saved registers (charged at the first suspension; later ones save as many), Error captured frames (up to MAX_FRAMES 1000, charged at capture), for-in key lists (exist before the object, so the alloc charge covers them) and pending-promise reaction lists (Reactions::push returns the growth). Allowed to report 0, with reasons in the test: fixed-size classes, Native/RegExp/Namespace/Module (engine or loader data; regex programs are TASK-103.52), Host and Intl. alloc_weight, gc, gc_native_frames, gc_safe_points, host_driven and promises tests pass.

Red first: the FinalizationRegistry loop aborted the process at a 96 GiB allocation. Audit of every Class found four more growable payloads (generator registers, error frames, for-in keys, promise reactions). Growth is charged where it happens. Commit 5ff369f. Gates: agnijs-js-vm and agnijs-js-regex suites green; clippy: no warnings on changed lines; test262 0 regressions in default, --precompile, --snapshot, --only-intl402, vs QuickJS-NG, the GC-stress slice and the JIT force and tiered slices.

Benchmarks (JIT off, 4 interleaved rounds against edbdb50, 26 workloads): geomean +0.23%. prop-write-alloc measured +2 to +4% against edbdb50 from this commit on; an early exit for the common classes in payload_bytes made it slower (77.6 vs 74.9 ms), so the cost follows code layout and is left as is.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Class::payload_bytes now reports FinalizationRegistry cells, DisposableStack resources, bound arguments, generator registers, captured error frames, for-in key lists and pending promise reactions, and growth after allocation is charged where it happens. memory_limit.rs: each repro under 16 MiB ends in OutOfMemory; every_growable_payload_is_counted sorts every Class into counted or allowed-to-be-zero with an exhaustive match. Commit 5ff369f.
<!-- SECTION:FINAL_SUMMARY:END -->
