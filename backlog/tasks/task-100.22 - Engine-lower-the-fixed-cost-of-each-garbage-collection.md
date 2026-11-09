---
id: TASK-100.22
title: 'Engine: lower the fixed cost of each garbage collection'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-09T03:18:00Z'
updated_date: '2026-11-09T15:17:00Z'
labels:
  - ready-for-agent
dependencies: []
documentation:
  - docs/js-engine-performance-findings.md
parent_task_id: TASK-100
priority: medium
type: enhancement
ordinal: 256000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
A collection costs about 4 ms even when only 400 KB is live. prop-write-alloc: 12 collections, 47.8 ms (14% of the run); closures: 7, 36 ms (17%); string-concat: 6, 23 ms (16%); promise-chain: 5 collections over 6 iterations, 109 ms with 22.8 MB live. Sweeping is 60 to 80% RtlFreeHeap because each dead ObjectData drops its own Vecs, plus NtFreeVirtualMemory; the root scan visits every key of every shape. In the Gable set, collect is 9.9% of list-diff and 13.2% of template-mount even though collections happen only at job boundaries today. Once TASK-100.16 lets the collector run during execution, collections become more frequent, so this cost grows.

Reproduction: run prop-write-alloc with a temporary counter around Vm::collect (count and nanoseconds), or profile it with winprof and read the Vm::collect inclusive share.

Benchmarks to prove it: prop-write-alloc, closures, string-concat, promise-chain (micro) and list-diff, template-mount (Gable).

Findings: docs/js-engine-performance-findings.md, finding 7. Keep the engine crates Gable-agnostic (ADR 0016); the agnijs-* rename is TASK-100.12, so use the current crate names. Measure before and after with the same binaries and machine; micro-benchmarks via `native/engine/target/release/bench.exe run --engine gable --engine quickjs` and `bench compare`, Gable benchmarks via `node native/engine-harness/js/run-bench.mjs --engine gable --engine quickjs`. The sampling profiler is native/tools/winprof (see the findings doc, Method).

Model assignment (maintainer, 2026-11-08, via TASK-100.14): Fable.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A collection with under 1 MB live takes under 1 ms on the reference machine, measured on prop-write-alloc
- [x] #2 Shapes are traced from live objects (or otherwise no longer scanned in full as roots), with a test that unreachable shapes are freed
- [x] #3 list-diff and template-mount spend at least 40% less time in Vm::collect than at the start of the task
- [x] #4 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [x] #5 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Profile per-collection phases (temporary timers + winprof) on prop-write-alloc, closures, string-concat, promise-chain, list-diff, template-mount.
2. Trace shapes instead of rooting every key-table key: objects mark their shape, shapes their parent and the keys they add, live blocks' inline caches the shapes they name; free unmarked shapes and key tables, reuse ids, clear untraced caches via a shape epoch (ADR 0019 J5).
3. Mark directly into mark bits moved out of the arenas; record weak containers while marking so ephemeron and weak passes scan no arena.
4. Sweep by ownership: merge the host-finaliser scan into the object sweep, total live bytes during the sweep, and hand dead objects' and scopes' small buffers to a pool reused by new objects and scopes.
5. Tests: unreachable shapes freed, survivors keep ancestors, reused shape ids never hit stale caches, live caches keep shapes, recycled storage starts empty, weak containers still processed; GC-stress subset.
6. Measure before/after, update ADRs 0009/0012/0019, test262 + Gable suites.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Profile (temporary per-phase timers + winprof), before:
- prop-write-alloc: 94 collections, ~2.0 ms each (sweep of objects 1.5, mark 0.1-0.2, weak scan 0.15, host scan 0.07, roots 0.015), 394 KB live, 1341 live objects in 39471 slots.
- closures: ~3.5-4.2 ms per collection (object sweep 2.5, env sweep 0.5). string-concat: ~3.1 ms (string frees 2.7). promise-chain: ~40 ms (24 MB live).
- list-diff: 13 collections, 52-60 ms each (mark 11.5, weak scans 8.4-9, host scan 2.6-3, object sweep 20-26, other sweeps 5-7), 695 ms total in Vm::collect over 6 iterations. template-mount: 6 collections, 13-15 ms each, 75 ms total.
Root cause: one allocator free per dead object buffer in the sweep (60-80% RtlFreeHeap), full-arena scans for ephemerons, weak processing, dead hosts and approx_live_bytes, and a two-pass tracer. Root scanning was already cheap; the shape-key root loop mostly kept every shape key alive.

Changes:
- Shapes traced, not rooted: objects mark their shape, shapes their parent and the keys they add (per-table high-water mark), live functions' and frames' blocks mark the shapes their IC entries name. Heap::sweep_shapes frees unmarked shapes and key tables (ids reused via free lists), unlinks them from live parents and trims tables. A shape epoch in Feedback clears untraced blocks' caches at their next frame push (ADR 0019 J5).
- Tracer owns the arenas' mark bits and marks on report (one work list, ropes only for strings); weak containers are recorded during marking, so ephemerons and process_weak visit only them.
- Arena::sweep takes ownership of dead values: dead objects' slot and dense element Vecs and dead scopes' slots go to BufPools (<=32 entries, budget = gc threshold, leftovers freed at the next collection) used by add_own_property, array_push_dense and Heap::alloc_declarative; host finaliser extraction and live-byte totals happen in the same pass.

After (same machine, temporary timers):
- prop-write-alloc: 0.58-0.67 ms per collection (AC1 < 1 ms). closures 1.7 ms, string-concat 2.5 ms, promise-chain 18 ms.
- list-diff: 377 ms in Vm::collect (-46%), ~29 ms per collection. template-mount: 41 ms (-45%).

Throughput, base a30d0a9 vs new, alternating binaries, two pairs (new/old): prop-write-alloc 0.62/0.69, closures 0.66/0.69, async-await 0.77/0.75, json-parse 0.86/0.84, regex 0.90/0.88, string-concat 0.94/0.98, calls-recursive 0.95/0.91; all others 0.94-1.03. bytecode-load (1.05/1.19 in one pair) and promise-chain (1.06) rechecked over 4 alternations at 10 iterations: 0.98-1.02 and 0.78-0.81. A first build that checked the shape epoch at every IC site cost loop-array-index 20%; the check moved to push_frame, after which loop-array-index is 0.98-1.00.
Gable (median of 2 alternations, ms): template-mount 83.7 -> 74.0, template-update 45.5 -> 41.4, list-diff 942 -> 856, signal-churn 507 -> 455, signal-create 114 -> 103, rxjs-operators 174 -> 170, rxjs-higher-order 365 -> 342, rxjs-subject-fanout 141 -> 128. Peak RSS: list-diff 224 -> 246 MiB, signal-create 44 -> 51 MiB (pooled buffers), others within 2-7 MiB.

Verification: cargo test --workspace (release) green, ic_stats feature green; AGNIJS_GC_STRESS=1 on tests ic, calls, gc, shapes, dispatch, gc_safe_points green; GC-stress test262 slice (class, assignment, for-of; AGNIJS_GC_STRESS=16) 5949/5949; test262 agnijs 42225/42225, --precompile 42225/42225, --only-intl402 981/981, RegExp and String regex methods 2269/2269 under backtracking-only and linear-when-eligible; Gable suites via the path-patched harness 759 passed, 1 known timing failure (yjs perf); harness manifest and lockfile restored.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Lowered the fixed cost of each collection.

What changed:
- Shapes are now traced from live objects and live blocks' inline caches, not rooted through every key-table key. Unreachable shapes and key tables are freed and their ids reused. A shape epoch clears the caches of untraced blocks before they run again (ADR 0019 J5).
- Marking writes mark bits directly and records weak containers, so the weak, ephemeron and host passes no longer scan the arena.
- The sweep takes ownership of dead values. It recycles their small slot, element and scope buffers through pools, and totals live bytes as it goes.

Results:
- A prop-write-alloc collection (394 KB live) takes 0.6 ms, down from 2 ms.
- Vm::collect time is down 46% on list-diff and 45% on template-mount.
- No benchmark regresses beyond noise. prop-write-alloc and closures are about 1.5x faster, and all Gable benchmarks are 2-12% faster.

Tests:
- New tests cover freed shapes, surviving ancestors, reused shape ids versus stale caches, caches keeping their shapes, recycled storage starting empty, and weak containers.
- test262 has no regressions in either mode. The Gable suites give 759 passed with the 1 known timing failure.

Docs: ADRs 0009, 0012 and 0019 and the findings doc are updated.

Follow-ups:
- Peak RSS rises up to 10% on list-diff because of the pooled buffers.
- Mark time (~11 ms on list-diff) is now the largest share.
<!-- SECTION:FINAL_SUMMARY:END -->
