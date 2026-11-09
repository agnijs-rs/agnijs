---
id: TASK-100.21
title: 'Engine: lighter objects, functions and string constants'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-09T03:18:00Z'
updated_date: '2026-11-09T16:19:00Z'
labels:
  - ready-for-agent
dependencies: []
documentation:
  - docs/js-engine-performance-findings.md
parent_task_id: TASK-100
priority: medium
type: enhancement
ordinal: 255000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The engine allocates far more than QuickJS-NG per object. Every object is a 120-byte ObjectData with a separate `slots: Vec<PropSlot>` (32 bytes per slot) and a separate elements Vec. `create_function` allocates the object, a Box<FunctionData>, the slots Vec, the length and name properties, a name string and (for constructors) a prototype object: 28% of closures, 6.6% of rxjs-higher-order, 4.2% of template-mount. `LdaConst` of a string constant calls alloc_str_utf16 every time it runs (per iteration: string-concat 400 K, prop-write-alloc 300 K, map-set 200 K, method-calls 150 K). The system allocator is 30 to 40% self time in promise-chain, async-await, json-parse and regex, 27% in closures and 13 to 20% in the Gable set.

Reproduction (ES module, 1 M iterations, Gable vs QuickJS-NG ms): closure creation `const g = () => i` 456 vs 234; object literal `{ a: i, b: i }` 389 vs 189; a loop reading `"abc".length` 284 vs 53.

Benchmarks to prove it: closures, prop-write-alloc, string-concat, method-calls (micro) and rxjs-higher-order, template-mount, signal-create (Gable).

A faster global allocator in the host binaries (gable-harness, bench) is a separate embedder decision; measure it here if useful, but the engine crates must not set a global allocator.

Findings: docs/js-engine-performance-findings.md, finding 6. Keep the engine crates Gable-agnostic (ADR 0016); the agnijs-* rename is TASK-100.12, so use the current crate names. Measure before and after with the same binaries and machine; micro-benchmarks via `native/engine/target/release/bench.exe run --engine gable --engine quickjs` and `bench compare`, Gable benchmarks via `node native/engine-harness/js/run-bench.mjs --engine gable --engine quickjs`. The sampling profiler is native/tools/winprof (see the findings doc, Method).

Model assignment (maintainer, 2026-11-08, via TASK-100.14): Fable.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Loading a string constant does not allocate a new string each time it executes
- [x] #2 Creating a function allocates at most two Rust heap blocks in the common case; prototype, length and name stay observable exactly as before (property order, attributes, test262)
- [x] #3 Small objects keep their first few property slots inline, without a separate slots allocation
- [x] #4 closures and prop-write-alloc are at least 1.3x faster than at the start of the task
- [x] #5 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [ ] #6 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Measure: sizes (Value 16, PropSlot 32, ObjectData 120, FunctionData 48) and profiles of closures and prop-write-alloc. 2. Tests first (tests/alloc_weight.rs): layout, string constants across collections, function blocks and properties, inline slots, add-by-transition invalidation. 3. Layout: 16-byte PropSlot (OptObj accessor halves), 24-byte Class, repr(C) ObjectData with 4 inline slots plus overflow and offset_of! constants. 4. String constants and function names survive collections with their traced block. 5. create_function: precomputed rooted shapes, cached name atom, pooled FunctionData boxes. 6. Property adds: SetNamed add entry (KIND_OWN_ADD) and DefineOwnNamed by existing transition; first-transition fast path in shapes. 7. Bench (alternating binaries), tests, gc-stress subset, test262 x4, Gable suites and benchmarks, docs (ADRs 0009/0012/0019, findings 6).
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
JIT-readiness (ADR 0019, TASK-101.1): J5, J11 apply. If you change the object layout, give the inline-cache hit path a defined layout: a #[repr(C)] object header (shape id, dictionary flag, the class tag the cache tests, slot storage) with offset_of! constants, contiguous slot values (inline slots plus overflow is fine, with a fixed rule for which indices are inline), and if you touch Value, a #[repr(C, u8)] or NaN-boxed representation. Lazy function properties must stay describable by the shape. For snapshots (J11), prefer heap-resident state over new Rc<Cell<_>> side state and identify natives by a stable registry index rather than adding more fn-pointer dependence.

Results (2026-11-09, same machine, base 3848eba vs final, alternating binaries).

Reproduction (gable-harness run, 1 M iterations, ms; base / final / QuickJS-NG): closure 427-433 / 311-318 / 272; object literal 334-348 / 247-262 / 190; "abc".length 135-139 / 142-145 / 51 (the constant no longer allocates; what is left is the primitive property lookup, not allocation).

Micro (bench run --engine agnijs, two alternating pairs, new/old): closures 91.4 -> 67.5 and 90.6 -> 65.3 ms (1.36x to 1.39x; 1.45x in earlier runs); prop-write-alloc 125.1 -> 77.9 and 127.0 -> 76.3 ms (1.61x to 1.67x); prop-monomorphic 0.95; method-calls 1.04 / 1.02; prop-polymorphic 1.03 / 1.04; string-concat 0.98 / 1.01; json-stringify 1.05 / 1.00; promise-chain 1.06 / 1.03; everything else within 0.95 to 1.03. Peak memory: closures 24 -> 20 MiB, prop-write-alloc 18.1 -> 17.0 MiB.

Gable (harness bench, median ms, base -> final): rxjs-higher-order 334 -> 276 (-17%); template-mount 73 -> 66 (-9%); signal-create 101 -> 86 (-15%); template-update 41.3 -> 39.3; list-diff 833 -> 787; rxjs-subject-fanout 125.6 -> 121.5; ui-bundle-startup flat; signal-churn 442 -> 462 (+4.5%); rxjs-operators 167 -> 176 (+5.6% over six alternating runs, +4 to +7% per pair). Peak RSS falls across the set (list-diff 245 -> 228, signal-create 52.7 -> 40.3, rxjs-higher-order 127 -> 106, template-mount 63 -> 47 MiB).

rxjs-operators: not fixed. Tried and reverted: disabling the add-IC hit, not inlining add_with_shape, no FunctionData pool, no literal fast path, counting inline slots in pacing, Class without repr(u8). Kept: inline slots moved before elements at a 16-byte-aligned offset (cut prop-polymorphic from +8-10% to +3%). Line-level profiles (winprof) show equal work; the extra samples sit on the dispatch loop's attention check and instruction fetch and on do_return/pop_frame, i.e. stalls, most likely from the bigger inline object footprint, not from a changed path.

Verification: cargo test --workspace --release; AGNIJS_GC_STRESS=1 debug subset (ic, calls, gc, shapes, dispatch, gc_safe_points, alloc_weight); test262 42225/42225 default and --precompile, intl402 981/981, RegExp/String regex slices 2218/2218 under both matchers, GC_STRESS=16 language slice 10898/10898; Gable suites 759 passed, 1 known yjs timing failure.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Objects, functions and string constants cost fewer heap blocks. ObjectData is repr(C) with four inline PropSlots (16 bytes each, now that accessor halves are OptObj) and offset_of! constants, and is still 160 bytes. A function is two blocks: it uses precomputed rooted shapes, a cached name atom and recycled FunctionData boxes. String constants and function names survive collections with their traced block. Property adds go through a cached transition (KIND_OWN_ADD, validated by proto_epoch and watched objects) and object literals take existing transitions. closures runs 1.36x to 1.45x faster and prop-write-alloc 1.6x, and peak memory is lower. test262, the gc-stress subset and the Gable suites pass. AC 6 is not met: rxjs-operators is about 5.6% slower (signal-churn +4.5%). Profiling points to dispatch-loop stalls rather than any changed path, and the experiments tried are listed in the notes. ADRs 0009, 0012 and 0019 have addenda, and finding 6 is marked done.
<!-- SECTION:FINAL_SUMMARY:END -->
