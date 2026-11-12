---
id: TASK-103.25
title: >-
  Memory limit: count ArrayBuffer backing stores, collection entries and large
  native allocations
status: To Do
assignee: []
created_date: '2026-11-12T06:25:00Z'
labels:
  - ready-for-agent
  - security
dependencies: []
references:
  - docs/security-model.md
parent_task_id: TASK-103
priority: high
ordinal: 289000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the security review (TASK-103.9, docs/security-model.md). The memory limit counts only what `Heap::charge` sees: object headers, slots, elements, dictionaries, strings, environments, symbols and BigInts (`heap.rs`, `ObjectData::extra_bytes` in `object.rs`, the sweep in `gc.rs`). ArrayBuffer and SharedArrayBuffer backing stores (`builtins/arraybuffer.rs`, `zeroed`) and the entries of Map, Set, WeakMap and WeakSet live in class payloads that nothing charges, and `memory_usage` leaves them out too. A probe with a 16 MiB limit allocated 64 ArrayBuffers of 4 MiB (256 MiB) and a Map of 2,000,000 entries without an error, while `memory_usage().heap_bytes` reported under 200 KB; the same limit stopped an array of 2,000,000 numbers. Separately, the limit is checked only at the dispatch loop's safe points, after a built-in has already allocated: `'x'.repeat(2**30 - 1)` (2 GiB of UTF-16), `padEnd`, `Array.prototype.join` through `StrBuilder`, and a single `new ArrayBuffer(4294967295)` all allocate in full before the next check, so one call can overshoot any limit by gigabytes, and without overcommit the system allocator aborts the process. Embedders that run untrusted code rely on the limit to keep a script from taking the host down.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 ArrayBuffer and SharedArrayBuffer backing stores, including resizable buffer growth, count towards the memory limit and memory_usage, and are released from the count when collected
- [ ] #2 Map, Set, WeakMap and WeakSet entries count towards the memory limit and memory_usage
- [ ] #3 A built-in that would allocate more than the remaining limit fails with the memory-limit error (or a RangeError) before allocating, for at least ArrayBuffer construction and resize, String repeat, padStart and padEnd, Array join and string concatenation
- [ ] #4 Tests show a 16 MiB limit stopping 256 MiB of ArrayBuffers, a 2,000,000-entry Map and a single 1 GiB repeat without the process allocating the full amount
- [ ] #5 docs/security-model.md and docs/c-api.md are updated to describe what the limit now covers
<!-- AC:END -->
