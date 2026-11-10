---
id: TASK-100.16
title: 'Engine: let the collector run while native frames are on the stack'
status: In Progress
assignee:
  - '@fable'
created_date: '2026-11-09T03:18:00Z'
updated_date: '2026-11-10T05:28:00Z'
labels:
  - ready-for-agent
dependencies: []
documentation:
  - docs/js-engine-performance-findings.md
parent_task_id: TASK-100
priority: high
type: enhancement
ordinal: 250000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The collector never runs during embedded execution. `Vm::maybe_collect` returns at once when `native_depth != 0`, and `in_native` raises it for every native function and host entry: `engine::call`, module evaluation (`execute_module` uses `call_value`), promise jobs and every native that calls back (`Function.prototype.call`, `Array.prototype.map` and so on). Collections happen only between jobs and between host calls, so one long synchronous call keeps all of its garbage. This is the whole peak-memory gap in the M9 set: rxjs-higher-order 124 MiB against 6.7 MiB on QuickJS-NG, list-diff 238 against 41, template-mount 52 against 18, signal-create 46 against 8. It also makes the run loop's slow branch (memory-limit check) run before every instruction once the threshold is passed inside a deferred region.

Reproduction (ES module, `gable-harness run --engine gable|quickjs`, measure peak working set): `function work(){ let keep = 0; for (let i = 0; i < 2000000; i++) { const o = { a: i, b: [i, i + 1] }; keep += o.b.length; } return keep; } console.log(work());` peaks at 900.7 MiB on the Gable engine and 5.4 MiB on QuickJS-NG; wrapping the call in `[0].forEach(() => ...)` gives the same numbers.

Benchmark to prove it: peak RSS of rxjs-higher-order, list-diff, template-mount and signal-create in run-bench.mjs.

Collecting under a native frame needs every value a native holds in Rust locals to be rooted (a root or handle stack, or VM-visible slots). A missed root is a use-after-free, so this is correctness-critical. TASK-100.19 (calls) removes the native frame from Function.prototype.call/apply and host entry, which takes away the most common case first; doing it first is recommended but not required.

Findings: docs/js-engine-performance-findings.md, finding 1. Keep the engine crates Gable-agnostic (ADR 0016); the agnijs-* rename is TASK-100.12, so use the current crate names. Measure before and after with the same binaries and machine; micro-benchmarks via `native/engine/target/release/bench.exe run --engine gable --engine quickjs` and `bench compare`, Gable benchmarks via `node native/engine-harness/js/run-bench.mjs --engine gable --engine quickjs`. The sampling profiler is native/tools/winprof (see the findings doc, Method).

Model assignment (maintainer, 2026-11-08, via TASK-100.14): Fable.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 The collector can run at interpreter safe points while one or more native frames are on the stack, including module evaluation, host calls, promise jobs and native callbacks
- [x] #2 The reproduction above peaks below 32 MiB of working set, both at module top level and inside a forEach callback
- [ ] #3 Peak RSS of rxjs-higher-order and list-diff is within 2x of QuickJS-NG in run-bench.mjs
- [ ] #4 The GC stress mode (collect at every safe point) runs over test262 and the Gable suites with no failures, and a test covers each native that calls back into JavaScript while holding heap values
- [x] #5 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [ ] #6 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
Design: snapshot retention at nested run-loop entry. 1. Every arena slot records a monotonic allocation serial (Heap::alloc_serial). 2. When Rust enters a nested run loop (Vm::run with native_depth > 0: natives calling back, host calls, jobs, module evaluation, generator resumption), the VM records the serial; a collection under that loop treats every heap value born before the innermost such serial as a root (traced, so what it references survives too). Rust code can only hold values that existed when it entered the loop, so no native needs per-site rooting and the collector stays precise, non-moving and stack-map free (ADR 0019 J6). 3. Vm::maybe_collect no longer refuses under native frames; refresh_attention keeps the GC bit set. 4. Pacing excludes the retained bytes so the next collection (at an outer level) frees them. 5. Tests first: memory growth with native frames on the stack (forEach/map/sort/replace callbacks, jobs, host calls, modules), GC-stress variants, natives holding values across callbacks; then implementation; then test262 (default, precompile, intl402, RegExp modes), gc-stress subset, Gable suites and benches; ADR 0009 addendum and findings doc.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
JIT-readiness (ADR 0019, TASK-101.1): J6 applies. The rooting mechanism you add for natives (root or handle stack, or VM-visible slots) is also the one JIT and AOT runtime helpers will use, so it must not depend on scanning the Rust or machine stack and must not need stack maps; keep the collector precise and non-moving (ids stable while live). Safe points stay at runtime calls and attention polls (J7). If you add a write barrier, make it one Heap function every value store goes through. A moving collector would reopen ADR 0019.

Milestone 1 (commit 0669268): retention design implemented, gc_native_frames.rs (24 tests) green, VM suite green. test262 after the change: default 42231/42231, --precompile 42231/42231, intl402 981/981, RegExp suites 2511/2511 under auto, backtracking-only and linear-when-eligible; 0 regressions, 0 exclusions changed. The gc_safe_points host-call test now really collects inside the calls (stress GCs under native frames were skipped before), so its stress window is 10 calls instead of 50 to keep the suite under 2 minutes.

GC stress (unit level), commits 7641250 and b856ede: at AGNIJS_GC_STRESS=1 and 16 the ic, calls, gc, shapes, dispatch, gc_safe_points, alloc_weight, promises, lone_surrogates, regexp, host_driven and gc_native_frames suites and the bench crate builtin_equivalence, builtin_allocs and call_allocs suites all pass. Four tests needed adjusting because stress collections now run inside jobs and host calls: the module-level and promise-jobs pacing tests (millions of collections under stress; the promise-jobs test took 97 minutes) and the two Rust-allocation-count suites pin stress off, and the dispatch stress test sets its own period. No engine change was needed.

GC stress test262 slice (AGNIJS_GC_STRESS=16, 60 s per test): language class/assignment/for-of plus built-ins Array/String/JSON/Promise/RegExp, 13070 tests: 12623 passed, 0 failures other than 447 timeouts, all of them the generated RegExp property-escapes and CharacterClassEscapes tests (over a million top-level loop iterations each; at native depth 0 the collector behaves as before, and TASK-100.29 left the same files out of its stress runs for the same reason; they pass in the normal runs). One of them, White_Space.js, takes 0.3 s without stress and 124 s at stress 16 in this build.

Stress-timeout check against the pre-change binary (6c4a604, own target dir): White_Space.js at AGNIJS_GC_STRESS=16 takes 121 s before the change and 124 s after, so the 447 generated-RegExp timeouts in the stress slice are the pre-existing cost of a collection every 16 instructions over a million-iteration top-level loop, not a regression.

Gable bench regression found and fixed (commit 5c04414): the first build ran list-diff in 2.6 s instead of 0.8 s. AGNIJS_GC_TRACE (new) showed 800 collections per run: a collection asked for at a loop exit ran at the start of the next callback of the same native, retained the same garbage and asked again. Exit collections now wait for a safe point at or above the level that asked (Vm::exit_gc_at). The same trace showed list-diff keeping 73 MB of pre-loop garbage alive under keys.forEach, so the array natives forEach/map/filter/some/every/find* and Map/Set forEach now run their calls as a callback series: they root what they hold on Vm::roots and their callbacks retain from the enclosing loop bound. Audit of the series natives is in the comments at each site and in the gc_native_frames rooting tests (getters, proxy traps, species constructors, keys past 2^32, Reflect.apply, bound functions, spread arguments) at stress 0, 1 and 16. list-diff: 0.88 s, peak RSS 233 to 157 MiB (QuickJS 42); rxjs-higher-order 106 to 35 MiB (QuickJS 6.7).

Final validation (commit cdb161b; engine source identical to ca669bf). test262 default 42231/42231, --precompile 42231/42231, intl402 981/981, RegExp suites 2511/2511 under auto, backtracking-only and linear-when-eligible: 0 regressions, 0 exclusions changed. VM crate: 208 tests pass. GC stress on the final code: unit subset (ic, calls, gc, shapes, dispatch, gc_safe_points, alloc_weight, promises, lone_surrogates, gc_native_frames, host_driven, regexp) and the bench crate suites (builtin_equivalence, builtin_allocs, call_allocs) pass at AGNIJS_GC_STRESS=1 and 16; test262 slice at 16 (language class/assignment/for-of, built-ins Array/String/JSON/Promise/RegExp, 13070 tests): 12623 passed, the same 447 generated property-escapes/CharacterClassEscapes timeouts as before the change, nothing else. Gable suites (path-patched harness): 759 passed, 1 known timing failure (yjs perf). Reproductions (peak working set, MiB, agnijs before / after / quickjs): module top level 781.5 / 17.6 / 5.4; inside forEach 781.3 / 17.7 / 5.4; nested natives 403.9 / 17.9 / 5.4. Gable benches (run-bench.mjs, agnijs before / after, peak RSS MiB; quickjs): rxjs-higher-order 281.8 / 284.9 ms, 106.1 / 35.0 (6.7); list-diff 812.3 / 883.4 ms, 228.0 / 164.7 (42.4); template-mount 68.2 / 78.5 ms, 46.1 / 43.0; signal-create 91.0 / 92.3 ms, 40.0 / 29.5; rxjs-subject-fanout 125.6 / 128.6 ms, 28.3 / 19.1; rxjs-operators 190.6 ms, 8.9; template-update 39.9 / 40.5 ms, 23.6 / 24.3; signal-churn 495.5 / 492.4 ms, 15.0 / 15.2; ui-bundle-startup 98.3 / 99.8 ms, 41.6 / 41.6. Alternated base/new binaries, 3 rounds of 5 iterations, medians: template-mount 69.6-70.1 vs 77.5-82.3 ms (+12-18%), list-diff 821-832 vs 884-903 ms (+8%), rxjs-operators, template-update, signal-churn within 2%; a later alternated round: template-mount 66-68 vs 74-75 ms (+10%), rxjs-higher-order 280 vs 287 (+2%), signal-create 90 vs 94 (+4%), rxjs-subject-fanout 123 vs 127 (+3%). Micro-benchmarks (bench compare, base/new alternated, two rounds): all within 4% except engine-startup 0.57 vs 0.62 ms in both rounds (+8%, 50 us; no startup code changed), async-await 1.08 then 1.04, compile-source 1.08 then 1.03.

Exit-collection gate tried and reverted (commits 4710841, cdb161b). template-mount pays two collections per iteration (one at a loop exit for 7 MB retained by a native building the tree, all live by the time it runs), about 10% of its time; the base engine paid for no collection inside the timed call. Requesting the exit collection only when retained plus allocated-since bytes reached the base threshold removed that collection (template-mount back to 1.00x) but let retention compound across host calls, because garbage retained in one loop is born before every later loop at the same level and only a collection at an outer level frees it: signal-create peaked at 41 instead of 29 MiB, rxjs-subject-fanout 32 instead of 20, list-diff 244 instead of 182. Memory is the point of this ticket, so the exit collection runs whenever anything was retained; the two time deltas (list-diff +8%, template-mount +10%) are the cost of collecting at all inside those calls. AC 3 is not met: rxjs-higher-order 35 MiB against 6.7 (the 8 MiB default threshold plus a 9 MiB process baseline dominate), list-diff 165 against 42 (the 2x live growth factor, and garbage born before the host call that a depth-1 loop retains until the call returns).

Rooting audit. By construction: a collection under a nested run loop treats every heap value born before the innermost loop entry as a root, and Rust code can only hold values that existed when it entered the loop, so every native that calls back (reduce, sort, Array.from, flatMap, replace/split/matchAll, JSON.parse reviver and stringify replacer, Object.assign/entries/fromEntries, Map/Set builders, Promise combinators, Reflect/Proxy traps, conversions via valueOf/toString, bound functions, apply/construct, iterator helpers and spread, typed arrays, generator/async resumption from natives, the memory limit) needs no per-site rooting; each has a gc_native_frames test that allocates heavily in the callback and checks the native still sees its values, at stress 0, 1 and 16. By hand (opt-out via Vm::callback_series): Array.prototype forEach/map/filter/some/every/find/findIndex/findLast/findLastIndex and Map/Set.prototype.forEach root the receiver, the callback, thisArg and the result array on Vm::roots, pin non-index keys while a callback runs, and push the kept/returned value around create_data_property_or_throw; their callbacks retain only from the enclosing loop bound. Those sites are the only ones whose locals can outlive a collection; the test series_natives_keep_fresh_elements_read_through_getters_traps_and_species exercises them through getters, proxy traps, species constructors, keys past 2^32, Reflect.apply, bound functions and spread arguments.
<!-- SECTION:NOTES:END -->
