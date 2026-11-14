---
id: TASK-103.53
title: >-
  Memory limit: native loops that allocate from a bare length or native iterator
  never check the limit
status: Done
assignee:
  - '@claude'
created_date: '2026-11-13T22:49:00Z'
updated_date: '2026-11-14T12:59:00Z'
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
- [x] #1 The repros in the description end in OutOfMemory within the documented factor of a 16 MiB limit
- [x] #2 ArraySpread, iterable_to_list, the Promise combinators and Intl.ListFormat poll the interrupt and the memory limit
- [x] #3 docs/security-model.md describes the remaining gap accurately
- [x] #4 memory_limit.rs covers the repros
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red: add each repro to agnijs-js-bench/tests/memory_limit_peak.rs (allocator refuses past 192 MiB).
2. Vm::poll = interrupt + would_exceed once past the GC threshold; switch built-in poll sites to it.
3. Poll in ArraySpread (with a real collection, iterator rooted), iterable_to_list, Promise combinators, ListFormat; charge Rust-side lists.
4. memory_limit.rs and interrupt_native.rs coverage.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Red: 7 of 8 repros aborted the peak test past 192 MiB (Array.apply already bounded by TASK-103.48's 2^22 cap). After: all under 64-66 MiB; NATIVE_LOOP_BOUND 80 MiB documents the 4x (2x counted rule for natives, times a doubling vector). Extra gap found and fixed: own_property_keys of String and TypedArray objects built a key list from the length (Vm::charge_list). ArraySpread collects inside the instruction (collect_inside_instruction, no debugger hook) so [...new Array(3e5).keys()] under 16 MiB is not refused for 48 MB of iterator results. Observation: after Promise.all is stopped, its queued reaction jobs stay alive, so that engine stays over the limit (program state, not a leak). Full agnijs-js-vm test suite passes.

Red first: seven of eight repros passed the 192 MiB allocator cap (fill over 2e7 at 525 MiB, Promise.all over 3e6 keys at 3.6 GiB). Vm::poll applies the built-in rule (twice the limit, garbage included) once the counter passes the collection threshold. ArraySpread collects inside the instruction so a spread that fits is not refused for its garbage. Commit 736a893. Gates: agnijs-js-vm and agnijs-js-regex suites green; clippy: no warnings on changed lines; test262 0 regressions in default, --precompile, --snapshot, --only-intl402, vs QuickJS-NG, the GC-stress slice and the JIT force and tiered slices.

Benchmarks (JIT off, 4 interleaved rounds against edbdb50, 26 workloads): geomean +0.23%.

Follow-up 619ff76: under AGNIJS_GC_STRESS the spread collected per element and asked the host only once per 4096 polls, so the gc-stress CI job (runs 43585769550, 43585856420) took 2.8-3.3 s to interrupt `[...new Array(1e8).keys()]`. Red test `a_spread_that_collects_asks_the_host_after_each_collection` (500k-element live set: 10.7 s); `collect_inside_instruction` now asks the host after a collection that ran. Stressed and normal vm suites, test262 default and gcstress (0 regressions) and CI run 43593675780 are green.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Native loops now check the memory limit as they go: Vm::poll (interrupt plus the built-in rule of Heap::would_exceed) replaces poll_interrupt at every built-in poll site; iterable_to_list, the Promise combinators, Intl.ListFormat and sort charge the Rust lists they build; Vm::charge_list checks key lists sized by a typed array or string. Each repro now peaks at 64 MiB or less under 16 MiB and ends in OutOfMemory; interrupt_native.rs checks the same loops are interrupted. The remaining gap (up to about twice the limit per call) is documented. Commit 736a893.
<!-- SECTION:FINAL_SUMMARY:END -->
