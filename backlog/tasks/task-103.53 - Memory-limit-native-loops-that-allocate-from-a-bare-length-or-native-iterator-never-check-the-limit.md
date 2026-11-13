---
id: TASK-103.53
title: >-
  Memory limit: native loops that allocate from a bare length or native iterator
  never check the limit
status: To Do
assignee: []
created_date: '2026-11-13T22:49:00Z'
labels:
  - bug
  - security
dependencies: []
parent_task_id: TASK-103
priority: medium
ordinal: 318000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the embedder-surface review of 2026-11 (docs/surface-review-2026-11.md), at commit fea680f.

Severity: medium (a memory-limit bypass). docs/security-model.md ("Built-ins that allocate many small values in one call") says such calls are "bounded by the input they read, which is already counted". In these cases the input is a bare length, a native iterator, or one value referenced N times, so nothing counted bounds the allocation. The limit is checked only at the next safe point, after the whole native loop has run.

Root cause: the limit is checked in `attention_slow` (crates/agnijs-js-vm/src/interp/mod.rs around line 746). `Vm::poll_interrupt`, which native loops call, checks only the interrupt flag, so no limit check or collection happens inside a native loop. Some loops do not poll at all: `ArraySpread` (interp/step.rs around line 1216), `iterable_to_list` (ops.rs around line 1418), and the Promise combinators (builtins/promise.rs around lines 977 and 1149). `Intl.ListFormat.format` (builtins/intl/listformat.rs around lines 85 to 104) builds its whole list first.

Minimal repros (`agnijs run --max-memory 16M`, debug build at fea680f) and peak working set:

    new Array(2e7).fill(0)                                       525 MiB
    Array.from({ length: 2e7 })                                  525 MiB
    Array.prototype.toSorted.call({ length: 2e7 })               1,136 MiB
    Array.apply(null, { length: 2e7 })                           833 MiB
    [...new Array(5e6).keys()]                                   1,394 MiB
    Promise.all(new Array(3e6).keys())                           3,688 MiB
    Object.entries(new Uint8Array(4e6))                          1,190 MiB
    new Intl.ListFormat('en').format(new Array(6e5).fill('a'.repeat(1000)))   1,861 MiB (32M limit)

Each ends in exit 3, but only after the peak. Lengths up to 2^32-1 are allowed, so on a larger machine the peak keeps growing until the allocator aborts.

Expected: out of memory within a small factor of the limit.

Suggested fix: give native loops a combined poll (for example `Vm::poll`, or extend `poll_interrupt`) that also compares the bytes allocated since the last collection against the limit and returns OutOfMemory. Call it from `ArraySpread`, `iterable_to_list`, the Promise combinators and `ListFormat`, and from every loop that creates values. Then narrow the documented gap to what remains.

Test that should catch it: memory_limit.rs tests for each repro above under a 16 MiB limit that assert OutOfMemory, and an engine-side accounting check (or a process peak check in CI) showing the peak stays within the documented factor.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 The repros in the description end in OutOfMemory within the documented factor of a 16 MiB limit
- [ ] #2 ArraySpread, iterable_to_list, the Promise combinators and Intl.ListFormat poll the interrupt and the memory limit
- [ ] #3 docs/security-model.md describes the remaining gap accurately
- [ ] #4 memory_limit.rs covers the repros
<!-- AC:END -->
