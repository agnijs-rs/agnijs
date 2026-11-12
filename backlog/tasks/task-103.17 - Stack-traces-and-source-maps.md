---
id: TASK-103.17
title: Stack traces and source maps
status: Done
assignee:
  - '@claude'
created_date: '2026-11-09T05:17:00Z'
updated_date: '2026-11-12T00:03:00Z'
labels:
  - needs-triage
dependencies: []
parent_task_id: TASK-103
ordinal: 273000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Complete stack trace support: Error.stack with accurate positions, Error.captureStackTrace and Error.stackTraceLimit, async stack traces across await, and source-map support so traces and the debugger show original positions for code that was compiled or transformed (including TypeScript through TASK-103.13). The host supplies source maps through the module loading interface. Model: Opus.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Error.stack format and Error.captureStackTrace follow the V8 convention closely enough for common tooling, with tests
- [x] #2 Async stack traces cross await, and a source map applied by the host changes the positions in Error.stack and in the debugger
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Error.prototype.stack accessor (error-stack-accessor proposal, test262 feature), captured frames in the [[ErrorData]] slot at creation, formatted lazily in the V8 format; Error.stackTraceLimit; Error.captureStackTrace; Error.prepareStackTrace with CallSite objects.
2. Async frames: walk the await chain from the bottom async frame (zero-cost: only at capture).
3. Host source maps: Engine::set_source_map and ModuleSource::SourceWithMap; Error.stack, Exception.stack and the debugger (ScriptInfo, CDP sourceMapURL and exception text) map positions; hardened bounded decoder with a fuzz test.
4. C API entry points (ABI 1.2), docs, ADR addendum; verify test262 (default, precompile, snapshot, intl402), GC stress, jit, no-intl, bench.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Verification (2026-11-11, Windows x86_64 i9-9900KF, rustc 1.99.0), tree 20f6b66 rebased on origin/main 21c510d:

test262 (release, 4 threads; total / passed / failed / skipped):

| run | total | passed | failed | skipped | vs baseline |
| --- | ---: | ---: | ---: | ---: | --- |
| default | 43192 | 43192 | 0 | 5718 | 0 regressions |
| --precompile | 43192 | 43192 | 0 | 5718 | 0 regressions |
| --snapshot (after the agnijs-snap-2 change) | 43192 | 43192 | 0 | 5718 | 0 regressions |
| --only-intl402 | 981 | 981 | 0 | 2412 | 0 regressions |
| GC stress 16, CI language slice | 5959 | 5959 | 0 | 20 | 0 regressions |
| JIT force, CI slice | 3615 | 3615 | 0 | 4 | 0 regressions |
| JIT on (tiered), CI slice | 3615 | 3615 | 0 | 4 | 0 regressions |
| no Intl, default | 43192 | 43192 | 0 | 5718 | 0 regressions |
| no Intl, --precompile | 43192 | 43192 | 0 | 5718 | 0 regressions |

Cargo gates, all passing: AGNIJS_GC_STRESS=1 tests of agnijs-js-vm and agnijs-js-compiler; the C API GC-stress subset; host_driven with AGNIJS_TEST_SNAPSHOT=1; agnijs-js-jit; agnijs-js-vm --features jit with AGNIJS_JIT=force (stack_traces included, so positions resolve through JIT frames); jit_workloads; no-Intl intl_feature and agnijs-capi; agnijs-capi with fault-injection; clippy -D warnings on agnijs-capi; header drift clean.

Snapshots: code restored from a snapshot reports its positions, and a host map registered after the restore applies (test stacks_resolve_in_code_restored_from_a_snapshot). Fix found while testing: the cached formatted stack was not written to the snapshot, contrary to ADR 0024; it is now (layout agnijs-snap-2, ADR 0022 and 0024 updated). Captured, unread frames are still dropped by design.

Bench (agnijs-js-bench, agnijs engine, 3 interleaved runs per side, median of run medians), HEAD 20f6b66 against 21c510d:

| workload | base (ms) | head (ms) | head/base |
| --- | ---: | ---: | ---: |
| loop-arith | 102.23 | 103.19 | 1.009 |
| loop-array-index | 91.59 | 93.21 | 1.018 |
| prop-monomorphic | 77.46 | 77.70 | 1.003 |
| prop-polymorphic | 120.04 | 119.62 | 0.997 |
| prop-write-alloc | 76.82 | 75.11 | 0.978 |
| string-concat | 61.75 | 61.32 | 0.993 |
| string-methods | 32.46 | 32.21 | 0.992 |
| array-methods | 315.52 | 316.13 | 1.002 |
| map-set | 117.51 | 117.96 | 1.004 |
| closures | 59.58 | 60.32 | 1.013 |
| calls-recursive | 54.80 | 57.11 | 1.042 |
| method-calls | 115.41 | 113.91 | 0.987 |
| regex | 100.76 | 99.81 | 0.991 |
| regex-tokenize | 29.26 | 28.91 | 0.988 |
| regex-long-input (layout effect, ignored) | 1.65 | 1.69 | 1.020 |
| regex-redos | 0.01 | 0.01 | 1.017 |
| json-stringify | 48.02 | 46.75 | 0.974 |
| json-parse | 44.07 | 44.58 | 1.012 |
| promise-chain | 15.17 | 15.26 | 1.006 |
| async-await | 14.54 | 14.67 | 1.009 |
| compile-source | 13.14 | 13.15 | 1.000 |
| bytecode-load | 2.04 | 2.10 | 1.026 |
| engine-startup | 0.61 | 0.61 | 0.997 |
| snapshot-startup | 0.52 | 0.53 | 1.030 |
| bundle-cold | 2.58 | 2.61 | 1.013 |
| bundle-snapshot | 2.01 | 2.08 | 1.034 |

Geomean head/base: 1.0052 excluding regex-long-input (1.0058 over all 26), within the 1% budget. calls-recursive is run-to-run drift: per-run medians 53.8 to 57.5 ms on base and 54.5 to 57.8 ms on head. No workload creates errors; capture runs only when an Error object is made, and the interpreter's call and return paths are unchanged.

Formatting: rustfmt applied only to hunks this task touched (main is not rustfmt-clean; whole-file formatting would reflow unrelated code).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Stack traces and host source maps for AgniJS.

- Error.prototype.stack is an accessor (error-stack-accessor). Every Error creation captures code and pc per frame into the [[ErrorData]] slot; positions are resolved and formatted in the V8 format at first read and cached (ADR 0024). Error.stackTraceLimit, Error.captureStackTrace (with the skip-to-function argument) and Error.prepareStackTrace with V8-style CallSite objects are supported. Async frames continue across await (await chain walked at capture). Columns count UTF-16 units; method calls are reported at the property name, as in V8.
- Host source maps: Engine::set_source_map(script, Option<map>) and ModuleSource::SourceWithMap, with a bounded Source Map v3 decoder and a deterministic fuzz test. Maps rewrite Error.stack, Exception stack frames and the debugger: ScriptInfo::source_map (takes precedence over sourceMappingURL); CDP sends it in Debugger.scriptParsed as a base64 data: URL and maps exception text.
- C API ABI 1.2: agnijs_set_source_map and agnijs_sink_set_source_map; header regenerated, fault-injection entries added.
- Snapshots keep a read (cached) stack; unread captures are dropped (layout agnijs-snap-2).

Verification: test262 default, precompile, snapshot, intl402, the GC-stress-16 slice, the JIT force and tiered slices, and no-Intl default and precompile all pass with 0 regressions. The GC-stress, JIT-forced, no-Intl, snapshot host-driven and C API (fault injection, clippy -D warnings, header drift) gates pass. Bench geomean against 21c510d: +0.52%, within 1%, no hot-path change. Tables are in the implementation notes.

Risks and follow-ups: natives have no frames, so V8 lines like "at Array.map (<anonymous>)" are absent. An error captured but never read before a snapshot restores with stack undefined (documented). The workspace is not rustfmt- or clippy-clean outside this task's code.
<!-- SECTION:FINAL_SUMMARY:END -->
