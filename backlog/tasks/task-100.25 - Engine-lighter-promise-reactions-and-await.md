---
id: TASK-100.25
title: 'Engine: lighter promise reactions and await'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-09T03:18:00Z'
updated_date: '2026-11-09T16:54:00Z'
labels:
  - ready-for-agent
dependencies: []
documentation:
  - docs/js-engine-performance-findings.md
parent_task_id: TASK-100
priority: medium
type: enhancement
ordinal: 259000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Promise and await internals allocate real function objects nobody can observe. new_promise_capability and new_native_closure build resolving functions and await reactions as full closures: 28.6% of async-await. GC is 32 to 34% of async-await and the allocator about 40%. Each pending promise holds about 1.1 KB live; promise-chain peaks at 161.7 MiB against 37.1 MiB on QuickJS-NG and runs 2.80x slower, async-await 3.24x.

Reproduction (ES module): `for (let i = 0; i < 100000; i++) Promise.resolve(i).then(() => c++)` takes 196 ms against 147 ms; the micro-benchmarks below show the larger gap.

Benchmarks to prove it: promise-chain, async-await (micro, time and peak RSS).

Job ordering must stay exactly as specified; test262's promise and async tests are the oracle.

Findings: docs/js-engine-performance-findings.md, finding 9b. Keep the engine crates Gable-agnostic (ADR 0016); the agnijs-* rename is TASK-100.12, so use the current crate names. Measure before and after with the same binaries and machine; micro-benchmarks via `native/engine/target/release/bench.exe run --engine gable --engine quickjs` and `bench compare`, Gable benchmarks via `node native/engine-harness/js/run-bench.mjs --engine gable --engine quickjs`. The sampling profiler is native/tools/winprof (see the findings doc, Method).

Model assignment (maintainer, 2026-11-08, via TASK-100.14): Fable.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Resolving functions and await reactions that script cannot observe are internal records, not function objects; resolving functions that script can observe behave exactly as before
- [x] #2 promise-chain and async-await are at least 1.4x faster and promise-chain peak RSS at least 30% lower than at the start of the task
- [x] #3 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [x] #4 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Measure base (8b7c05f): bench promise-chain / async-await, reproduction, winprof profile of async-await. 2. Tests first: tests/promises.rs (job-order characterisation for await/then/thenables/async generators/for-await/combinators/subclasses, visible resolving functions, objects allocated per await and per then, live objects per pending link, GC stress while suspended) and a debugger test stepping over await/yield with a breakpoint after resumption. 3. Internal capabilities: a %Promise% capability whose functions nobody can see keeps resolve/reject empty and settles the promise directly; real resolving functions only where script can reach them (executor, thenable job, combinators, withResolvers, subclasses). 4. Reaction handlers become an enum (default, callable, await resumption, async-generator return) so await registers records, not closures; await of a non-thenable skips the wrapper promise only where unobservable. 5. One reaction list per promise with the first reaction inline. 6. Measure (alternating binaries), tests, gc-stress subset, test262 x4, Gable suites and benchmarks, docs (findings 9b, ADR addenda where layout changed).
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Measurements (i9-9900KF, base = origin/main 8b7c05f, new = this change, binaries alternated, medians of runs):
- promise-chain: 38.4 ms -> 18.3 ms (2.10x; 37.4 -> 15.9 ms in a 10-iteration run, 2.35x); peak RSS 134.2 -> 57.3 MiB (-57%).
- async-await: 58.2 ms -> 16.0 ms (3.64x; 59.3 -> 15.4 ms in a 10-iteration run); peak RSS 21.9 -> 24.3 MiB.
- Reproduction (ES module via gable-harness run, 100 000 Promise.resolve(i).then(() => c++)): 243 ms -> 104 ms, QuickJS-NG 198 ms.
- Other micro-benchmarks (two alternating full runs, then three more alternating runs of the outliers): all within noise; method-calls 119.1 vs 120.3 ms, calls-recursive 57.3 vs 55.5 ms, compile-source 20.0 vs 20.5 ms, bytecode-load 2.10 vs 2.09 ms, engine-startup 0.56 vs 0.55 ms after rerun. None regresses by more than 5% on both median and min.
- Gable benchmarks (two alternating runs, median of medians): rxjs-operators 175.1 -> 176.7 (+0.9%), rxjs-higher-order 277.5 -> 278.1, rxjs-subject-fanout 120.8 -> 123.9 (+2.6%), signal-churn 466.0 -> 461.7, signal-create 86.4 -> 85.4, template-mount 66.1 -> 66.3, template-update 39.2 -> 38.9, list-diff 785.4 -> 787.1, ui-bundle-startup 137.4 -> 136.6 ms; RSS unchanged.

What changed: a capability for %Promise% that script cannot see (then, Promise.resolve/reject/try, async functions and generators) is internal, with no resolving functions; the engine settles it once through the resolve steps in its realm inside a native frame. Reaction handlers are an enum (default, callable, await resumption, async-generator return) so await and AsyncGeneratorAwaitReturn register records, not native closures; awaiting a primitive skips the wrapper promise. One ThenRecord per then, first reaction inline. Visible resolving functions (executor, thenable job, combinators, withResolvers, subclasses and foreign constructors) are unchanged. The native-frame GC deferral (TASK-100.16) was not touched.

Tests: new crates/agnijs-js-vm/tests/promises.rs (job-order characterisation with and without GC stress, visible resolving functions, objects per await/then, live objects per pending link, collection at every safe point while suspended); debugger test stepping over await and yield with a breakpoint after resumption. cargo test --workspace --release ok; promises/debugger/gc/host_driven ok in debug; AGNIJS_GC_STRESS=1 ic, calls, gc, shapes, dispatch, gc_safe_points, alloc_weight, promises ok; AGNIJS_GC_STRESS=16 test262 slices (class, assignment, for-of, object, Promise, await, async functions and generators, for-await-of, generators, yield): 7119 + 10155 + 4817 all passing. test262 agnijs 42225/42225, --precompile 42225/42225, intl402 981/981, RegExp/String-regex slices 2505/2505 under backtracking-only and linear-when-eligible; 0 regressions. Gable suites (path-patched harness, manifest and lockfile restored): 758 passed with the known yjs timing failure plus a flaky highlight.test 2 s timing budget (synchronous tokenizer, listed in suites-expected as timing); highlight.test passed on two reruns, so 759 + 1 known.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Unobservable resolving functions and await reactions are now internal records: an internal %Promise% capability without resolving functions, reaction handlers as an enum (default, callable, await resumption, async-generator return), one record per then with the first inline, and no wrapper promise when awaiting a primitive. promise-chain 38.4 -> 18.3 ms (2.1x) and 134 -> 57 MiB peak RSS (-57%); async-await 58.2 -> 16.0 ms (3.6x); the reproduction 243 -> 104 ms (QuickJS-NG 198 ms). No micro or Gable benchmark regresses beyond noise. test262 42225/42225 default and --precompile, intl402 981/981, RegExp slices pass under both matchers, GC-stress subset and Gable suites pass. Findings doc 9b marked done; ADR 0019 addendum: promise reactions are data.
<!-- SECTION:FINAL_SUMMARY:END -->
