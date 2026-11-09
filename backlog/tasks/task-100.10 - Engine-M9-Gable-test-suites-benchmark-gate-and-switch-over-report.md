---
id: TASK-100.10
title: 'Engine M9: Gable test suites, benchmark gate and switch-over report'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-07T22:37:00Z'
updated_date: '2026-11-09T02:47:00Z'
labels:
  - ready-for-agent
dependencies:
  - TASK-100.9
documentation:
  - docs/js-engine-spec.md
parent_task_id: TASK-100
ordinal: 211000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The Gable engine replaces the interim engine as soon as it is usable: spec section 11 conditions 1, 2 and 4 (test262, Gable's own suites, first-version debugger). Performance is reported, not gated; the maintainer has committed to adopting the engine and gaps are fixed afterwards (TASK-100.14). No maintainer sign-off is required for the switch.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A benchmark set (signal churn, RxJS pipelines, the template interpreter, list diffing, gable-ui bundle startup) exists and the interim engine's baseline and the Gable engine's numbers are recorded and reported, with no tolerance gate
- [x] #2 Gable's own test suites (core, router, forms, store and the rest) run under the Gable engine in CI
- [x] #3 A report evaluates each switch-over condition in spec section 11 and is written for the maintainer to read as a baseline
- [x] #4 If conditions 1, 2 and 4 hold, the Gable engine is made the default with the interim engine still selectable for one release; if any does not hold, the report says which and why, and the default is left unchanged
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Gable-side harness crate native/engine-harness (gable-engine-harness, own Cargo workspace and lockfile, path deps on gable-engine-api, gable-js-vm, gable-engine-quickjs; nothing added under native/engine, ADR 0016). It is a minimal embedder: loads one ESM bundle per fresh engine through Host::load_module, supplies the host primitives the suites need (setTimeout/setInterval/clear*, performance.now, a monotonic clock that skips idle waits) and runs a jobs-then-timers event loop. Engine choice lives here: --engine gable|quickjs, default gable once conditions 1, 2 and 4 hold, quickjs kept selectable for one release.
2. JS side under native/engine-harness/js: a vitest-compatible runtime shim (describe/it/test + each/skip/only/todo, hooks, expect with the matchers the suites use, asymmetric matchers, vi.fn/spyOn/fake timers/stubGlobal, expectTypeOf no-op), a host prelude of WinterCG stand-ins the suites touch (queueMicrotask, structuredClone, URL, Response/Headers, AbortController, TextEncoder, EventTarget...) installed only where missing, a rolldown bundler that turns each in-scope test file into one ESM bundle with the vitest.config aliases, and a runner that executes bundles on gable, quickjs and node (node validates the shim) and compares against a checked-in expectations file.
3. Suites manifest: in scope = every test file of the runtime packages that needs neither a DOM (spec 8: the engine provides none) nor Node built-ins (tooling that runs in Node, not in an app); out-of-scope files listed by rule with a reason. Covers core, testing (router, forms, http, i18n, hmr...), store, template-format, plus the non-DOM files of ui, ui-primitives and the rest.
4. Fix engine bugs the suites expose in the engine crates (test-first in gable-js-vm tests or test262-style), keeping test262 baselines (gable, --precompile, --only-intl402) at zero regressions; Gable-side issues get the minimal fix.
5. Gable benchmark set under native/engine-harness/bench: signal churn, RxJS pipelines, template interpreter, list diffing, gable-ui (@gable/ui) bundle startup; harness bench mode times them per engine in child processes; record the interim baseline and Gable numbers, report the gap, no gate; reuse the TASK-100.13 micro baseline in the report.
6. CI workflow .github/workflows/engine-suites.yml: build the harness, bundle, run suites under gable (gate) and quickjs (reported).
7. Switch-over report docs/js-engine-switch-over.md evaluating conditions 1-5 of spec section 11; if 1, 2 and 4 hold, make gable the default with quickjs selectable for one release.
Test boundaries (chosen without user input): Rust unit/integration tests for the harness event loop, timers, idle clock and engine selection against both engines; a shim self-test fixture suite with known pass/fail outcomes run on all three runtimes; engine bug fixes get regression tests in gable-js-vm. Benchmarks are not run under tests.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Suites (condition 2): native/engine-harness runs 79 in-scope test files (760 tests) bundled with rolldown in a fresh engine per file; 118 files out of scope (72 need a DOM, 46 are Node tooling), listed by rule with run-suites.mjs --list. Results: node 760/760 (shim check, counts match vitest's JSON reporter), gable 758/760, quickjs 749-750/760. The two gable failures are V8 wall-clock budgets (yjs perf 25 ms/change, highlight 2 s/input), recorded as timing: entries and counted under condition 3 (decision flagged in the report).
Engine bugs fixed (test-first, test262 unchanged at 42225/42225, precompile 42225/42225, intl402 981/981): sticky regex scanned the rest of the input on failure (gable-js-regex, anchored (?:P)()| wrapper); regex input cache thrashed by short strings (gable-js-vm, 4-entry cache for >=256 units); the collector never ran under an event loop because every job and host call runs as a native frame (gable-js-vm, safe points between jobs and on entry to Engine::call/construct; tests/gc_safe_points.rs). Gable-side: core http guards FormData/Blob (spec section 8 has neither); harness prelude adds Blob/File stand-ins for Zag; bundler defines process.env.NODE_ENV.
Benchmarks (condition 3): bench/baseline.json, 2 warmup + 10 iterations, i9-9900KF. Gable/QuickJS geomean 2.89x (1.64x list-diff to 4.19x rxjs-operators); ui-bundle-startup 145 vs 69 ms; peak RSS 1.4x-18x higher (collection still deferred inside one native frame; TASK-100.14). Before the GC fix list-diff peaked at 3.5 GiB.
Verification: cargo test --workspace --locked (native/engine) pass; harness cargo test --release --locked pass; run-suites on node/gable/quickjs exit 0; vitest packages/core + testing/http pass except public-types, which fails only because this worktree's node_modules has no typescript (tsc from the main checkout typechecks core cleanly).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Engine M9: the Gable engine is the default, and QuickJS-NG can still be selected for one release.

What changed
- Added native/engine-harness (gable-engine-harness), a Gable-side embedder with its own workspace. It sits outside native/engine, per ADR 0016. It contains:
  - EngineChoice, which defaults to Gable, with --engine quickjs still available;
  - an event loop and host;
  - a vitest-compatible shim and a host prelude;
  - a rolldown bundler;
  - run-suites.mjs, which compares results against suites-expected.json;
  - a benchmark set (signals, RxJS, template mount/update, list diffing, @gable/ui startup) with run-bench.mjs and a recorded baseline.json.
- The CI workflow engine-suites.yml runs the suites under node, gable (the gate) and quickjs on Ubuntu and Windows. It also runs a bench job that is reported only.
- docs/js-engine-switch-over.md evaluates spec section 11 conditions 1 to 5.

Engine fixes (test262 unchanged: 42225/42225 from source and precompiled, 981/981 intl402)
- Sticky regexes no longer scan the rest of the input on a failed match.
- The regex input cache no longer recopies long strings after a short one is matched.
- GC safe points now run between jobs and on entry to host calls. Before this, an event-loop embedder never collected, and list-diff reached 3.5 GiB.

Gable-side fix
- core http now checks that FormData and Blob exist before using them.

Results
- Suites: gable passes 758/760. The 2 failures are V8 wall-clock budgets, which the report counts under condition 3.
- Benchmarks: Gable/QuickJS geomean is 2.89x. Peak memory is 1.4x to 18x higher.

Decisions for the maintainer (detailed in the report)
- Whether to count the timing budgets as performance.
- Whether Blob and File become section 8 host globals.
- rolldown is a transitive dependency.

Follow-ups for TASK-100.14
- Collection is still deferred inside a single native frame.
- A failed exec of a non-multiline ^ regex still scans the whole long input.
<!-- SECTION:FINAL_SUMMARY:END -->
