---
id: TASK-103.25
title: >-
  Memory limit: count ArrayBuffer backing stores, collection entries and large
  native allocations
status: Done
assignee:
  - '@claude'
created_date: '2026-11-12T06:25:00Z'
updated_date: '2026-11-12T19:24:00Z'
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
- [x] #1 ArrayBuffer and SharedArrayBuffer backing stores, including resizable buffer growth, count towards the memory limit and memory_usage, and are released from the count when collected
- [x] #2 Map, Set, WeakMap and WeakSet entries count towards the memory limit and memory_usage
- [x] #3 A built-in that would allocate more than the remaining limit fails with the memory-limit error (or a RangeError) before allocating, for at least ArrayBuffer construction and resize, String repeat, padStart and padEnd, Array join and string concatenation
- [x] #4 Tests show a 16 MiB limit stopping 256 MiB of ArrayBuffers, a 2,000,000-entry Map and a single 1 GiB repeat without the process allocating the full amount
- [x] #5 docs/security-model.md and docs/c-api.md are updated to describe what the limit now covers
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red tests: crates/agnijs-js-vm/tests/memory_limit.rs (behaviour) and crates/agnijs-js-bench/tests/memory_limit_peak.rs (a peak-tracking global allocator capped at 192 MiB proves nothing large is allocated).
2. Count class payloads in ObjectData::extra_bytes (Class::payload_bytes): ArrayBuffer/SharedArrayBuffer store length, MapData::heap_bytes (live entries x MAP_ENTRY_BYTES + string/BigInt key contents). Allocation, sweep and memory_usage then all include them.
3. Charge growth where it happens: resizable resize/grow (resize_bytes, exact capacity), map_insert for every collection insert; Set methods allocate their result object first so additions are counted.
4. Heap::would_exceed / Vm::check_memory: refuse (Fault::OutOfMemory) before a large allocation when live + request > limit or everything counted + request > 2x limit. Called from zeroed (all buffer allocation), resize/grow, repeat, padStart/padEnd, join separators (Array, TypedArray, toLocaleString), every builder step via check_string_len, and Vm::concat for ropes (the flat buffer they become).
5. WeakMap/WeakSet compact tombstones (no iterators), so dying keys do not grow them.
6. Docs: security-model.md memory limit section and checklist, c-api.md.
7. Gates: clippy, own-hunk rustfmt, test262 (4 modes) + QuickJS-NG baseline, GC-stress slice, benchmarks within 1%, memory benchmarks.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Known gap, out of scope here: a deleted Map/Set entry leaves a tombstone slot (kept so live iterators keep their place) until the collection dies; the count covers live entries only, so a set/delete loop grows the process ~32 B per round without reaching the limit. Tracked as TASK-103.32 and referenced from docs/security-model.md. WeakMap/WeakSet (no iterators) now compact their tombstones.

Red run (old code): 10 of 17 tests in tests/memory_limit.rs failed; repeat, pad and join "passed" only because the post-call safe point caught them after allocating in full. The peak test aborted with "memory allocation of 2147483648 bytes failed". Green: 17/17 plus the 73-byte entry test, peak 3/3 (each refusal under 48 MiB), C API test.

Pacing side effect: a 150k-entry Map or Set now counts about 11 MiB live, and the GC threshold grows with live bytes (200%), so the callback-series test in gc_native_frames.rs saw 122,009 live objects against its 120,000 bound for the Map case. That is more garbage between collections, not a leak: with no collection under the frame all 300k would be live. The Map and Set cases now use a 150,000 bound, with a comment.

Rebased onto 8459ae4 (TASK-103.28) before the final gates; doc conflicts resolved, keeping 103.28's interrupt text.

Gates on the rebased tree:
- clippy: no warnings on my hunks; main has 27 pre-existing clippy errors under rustc 1.99, so -D warnings fails on lines already there.
- rustfmt: own hunks only.
- cargo test --workspace --no-fail-fast: green.
- test262 0 regressions in default, --precompile, --snapshot and --only-intl402; QuickJS-NG baseline 0 regressions.
- AGNIJS_GC_STRESS=1 vm and compiler tests green; GC_STRESS=16 test262 slice 0 regressions.
- JIT: agnijs-js-jit tests, VM suite with AGNIJS_JIT=force, jit_workloads, test262 slice forced and tiered: all green.
- Benchmarks (agnijs, 3 interleaved runs of 7 iterations each against 8459ae4): geomean -0.68% by min of medians, +0.05% by median of medians; peak RSS within 0.2 MiB on every workload.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
The memory limit now counts ArrayBuffer and SharedArrayBuffer stores, including resizable growth, and Map, Set, WeakMap and WeakSet entries (73 bytes each plus string and BigInt key contents). memory_usage reports them, and they leave the count when collected. Built-ins that allocate one large block check the limit first and fail with the uncatchable OutOfMemory instead of allocating: buffer allocation, resize and grow, repeat, padStart, padEnd, join, toLocaleString, string builders and rope-making concatenation. A request is refused when the last collection's live size plus the request passes the limit, or when everything counted since plus the request passes twice the limit.

Verification: under a 16 MiB limit, tests stop 256 MiB of buffers, a 2,000,000-entry Map and a 1 GiB repeat. A counting allocator shows each refusal stays under 48 MiB. All gates are green (see notes). Benchmark geomean is within 1% and memory benchmarks are unchanged.

Docs: security-model.md and c-api.md.

Risks and follow-ups:
- Sizes are approximate, and the process can use more than the limit (the peak test measured under 3x).
- Map and Set tombstones are not counted (TASK-103.32).
- A heap with large collections now collects less often, because the GC threshold grows with the bytes now counted.
<!-- SECTION:FINAL_SUMMARY:END -->
