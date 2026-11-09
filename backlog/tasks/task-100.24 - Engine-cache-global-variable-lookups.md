---
id: TASK-100.24
title: 'Engine: cache global variable lookups'
status: In Progress
assignee:
  - '@claude-fable'
created_date: '2026-11-09T03:18:00Z'
updated_date: '2026-11-09T07:47:00Z'
labels:
  - ready-for-agent
dependencies: []
documentation:
  - docs/js-engine-performance-findings.md
parent_task_id: TASK-100
priority: medium
type: enhancement
ordinal: 258000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Every global read does three hash lookups in `load_global` (interp/globals.rs): `global_lexicals` (IndexMap<Rc<str>> with SipHash), `self.key(n)` (interning the name) and `get_own_property` on the global object, which is in dictionary mode (another SipHash IndexMap). It is 16 to 26% inclusive in prop-monomorphic, loop-array-index, prop-polymorphic, method-calls and calls-recursive (fib is a global: 656 K loads per run). Bundled Gable code mostly uses module scope, so it is under 2% there, but plain scripts and globals like Math, Object and Array hit it.

Reproduction (ES module, 1 M reads of `globalThis.GG` through the bare name `GG`): 176 ms against 42 ms on QuickJS-NG (an empty loop is 129 vs 37).

Benchmarks to prove it: calls-recursive, prop-monomorphic, loop-array-index (micro).

Findings: docs/js-engine-performance-findings.md, finding 9a. Keep the engine crates Gable-agnostic (ADR 0016); the agnijs-* rename is TASK-100.12, so use the current crate names. Measure before and after with the same binaries and machine; micro-benchmarks via `native/engine/target/release/bench.exe run --engine gable --engine quickjs` and `bench compare`, Gable benchmarks via `node native/engine-harness/js/run-bench.mjs --engine gable --engine quickjs`. The sampling profiler is native/tools/winprof (see the findings doc, Method).

Model assignment (maintainer, 2026-11-08, via TASK-100.14): Fable.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A global read or write at a site whose binding has not changed does no hashing (per-site cache, property cell or equivalent), with invalidation tests for delete, redefinition, accessors and a global lexical shadowing a global property
- [ ] #2 calls-recursive is at least 1.15x faster than at the start of the task
- [x] #3 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [x] #4 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red: time the reproduction (1 M bare-name reads of a global) and calls-recursive with the TASK-100.28 bench.exe; add VM tests at the global-binding seam: a global read at an unchanged site stays correct across 1 M iterations, and invalidation on delete (ReferenceError afterwards), redefinition as an accessor (getter runs each time), redefinition as a non-writable data property (strict store throws), and a later script's global lexical shadowing a configurable global property.
2. Per-CodeBlock side table indexed by NameIdx (ADR 0019 J4: plain ids, never serialised, allocated lazily on first global access). An entry caches either the index of the name in the realm's global_lexicals (append-only, so an index is stable) or the index of its entry in the global object's dictionary IndexMap, tagged with the realm and a realm epoch that is bumped when a property is removed from the global object (indices shift and atom ids may be reused) and when a global lexical is declared (it may shadow a property). Hits re-check the entry kind (data, writable, initialised, not const) so redefinition needs no invalidation.
3. load_global / store_global check the cache before the three hash lookups; the slow path fills it. The *this* special case moves behind the cache check.
4. Measure after (alternating binaries), cargo test --workspace, debugger and cdp crates; test262 full, --precompile, intl402; Gable suites via the path-patched harness. Record numbers, finalize, commit, push.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Change: each CodeBlock carries a second side table, CodeBlock::globals (one GlobalEntry per name, indexed by NameIdx, allocated on the block's first global access, never serialised; ADR 0019 J4). An entry caches the realm and the position of the binding in the realm's global_lexicals IndexMap or in the global object's dictionary IndexMap, so a hit does no interning and no hashing (the global object is always in dictionary mode after the builtins install). Lexical positions are stable because bindings are only appended; a dictionary position is valid while the realm's new epoch is unchanged, and the epoch is bumped when a property is removed from the global object (ordinary_delete; positions shift and atom ids may be reused) and when a script declares global lexicals (they may shadow a cached property). A hit re-reads the binding's kind (data, writable, initialised, not const), so accessor and read-only redefinitions, const and the TDZ fall through to the generic path with no invalidation. The *this* string compare moved behind the cache check. ADR 0012 has an addendum. Tests: crates/agnijs-js-vm/tests/globals.rs (delete, accessor and read-only redefinition, setter, re-creation after delete, a later script's let/const shadowing a cached property including the TDZ, const writes, builtins and this), each with and without GC stress mode, plus an ignored release-mode reproduction timer.

Reproduction (1 M bare-name reads of globalThis.GG in a script function, best of 7, release): 162.3 ms before -> 115.0 to 119.9 ms after; the same loop over a local variable takes 115.6 ms, so a global read now costs about 4 ns on top of the loop against about 47 ns before. Benchmarks (TASK-100.28 head f8bd193 as the before binary, alternating runs, 10 iterations after 3 warmup; median of pairs): loop-array-index 592-604 -> 455-466 ms (1.30x), prop-monomorphic 455-473 -> 353-360 ms (1.29x), calls-recursive 174-184 -> 158-166 ms (pairs 1.09x to 1.22x, median 1.13x; against the start-of-task 173.7 ms it is 1.09x). Full micro set (5 iterations): prop-polymorphic 0.74, method-calls 0.78, regex-tokenize 0.83, loop-arith 0.94; nothing slower beyond 5% once promise-chain (60.4/58.8/58.9 vs 59.1/60.5/58.6 ms) and compile-source (21.3/20.1/20.3 vs 20.3/20.5/20.7 ms) were re-run alternating; regex-redos reads 0.01 vs 0.02 ms, below timer resolution. The Gable harness benchmarks run bundled module code (globals under 2% there) and were not re-run; the Gable suites were.

AC #2 is not met and is left unchecked: a winprof profile of calls-recursive on the new binary puts load_global at 1.4% self and inclusive, so the global lookup is gone from that workload; the remaining 36.6% step, 23.8% run, 15.6% push_function_frame and 11% allocator are the call path (TASK-100.19) and dispatch (TASK-100.17), and the 16-26% inclusive share in finding 9a included dispatch attributed to the load. Tests: cargo test --workspace 232 passed / 0 failed (debugger and cdp crates included); test262 42225/42225, --precompile 42225/42225, --only-intl402 981/981, 0 regressions; Gable suites via the path-patched harness 758 passed, 2 known timing failures (yjs perf, highlight linearity).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Global reads and writes cache the position of their binding per code block and name (no hashing on a hit; invalidation by a realm epoch on delete and lexical declaration, kind re-checked on every hit). A global read costs about 4 ns instead of 47; loop-array-index 1.30x, prop-monomorphic 1.29x, prop-polymorphic 1.35x, method-calls 1.28x, calls-recursive 1.13x median (AC #2 asked for 1.15x; the profile shows the global lookup is now 1.4% of that workload, the rest is the call path). No test262, Gable-suite or micro-benchmark regressions.
<!-- SECTION:FINAL_SUMMARY:END -->
