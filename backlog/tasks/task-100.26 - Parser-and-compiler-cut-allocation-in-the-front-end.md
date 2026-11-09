---
id: TASK-100.26
title: 'Parser and compiler: cut allocation in the front end'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-09T03:18:00Z'
updated_date: '2026-11-09T17:33:00Z'
labels:
  - ready-for-agent
dependencies: []
documentation:
  - docs/js-engine-performance-findings.md
parent_task_id: TASK-100
priority: medium
type: enhancement
ordinal: 260000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Compiling is allocation-bound. compile-source is 2.30x QuickJS-NG and ui-bundle-startup (the @gable/ui bundle) 2.07x; in ui-bundle-startup, compiling the module graph is 74% of the time (parse 38%, compile 26%). RtlAllocateHeap plus RtlFreeHeap are 36% self time in compile-source and 32% in ui-bundle-startup: boxed AST nodes, Rc<str> names and SipHash HashMaps in scope analysis. A fat-LTO, one-codegen-unit build made compile-source about 15% faster (other workloads did not move), so the release profile is worth checking here too.

Reproduction: profile `bench child --engine gable --workload compile-source` or `gable-harness startup --engine gable native/engine-harness/out/bench/ui-startup.mjs` with winprof and read the allocator share.

Benchmarks to prove it: compile-source (micro) and ui-bundle-startup (Gable).

Findings: docs/js-engine-performance-findings.md, finding 9c. Keep the engine crates Gable-agnostic (ADR 0016); the agnijs-* rename is TASK-100.12, so use the current crate names. Measure before and after with the same binaries and machine; micro-benchmarks via `native/engine/target/release/bench.exe run --engine gable --engine quickjs` and `bench compare`, Gable benchmarks via `node native/engine-harness/js/run-bench.mjs --engine gable --engine quickjs`. The sampling profiler is native/tools/winprof (see the findings doc, Method).

Model assignment (maintainer, 2026-11-08, via TASK-100.14): Fable.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 compile-source and ui-bundle-startup are at least 1.3x faster than at the start of the task
- [x] #2 The parser's existing tests and the test262 parse-error tests still pass, with identical error messages and positions
- [x] #3 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [x] #4 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Measure: compile-source and bytecode-load (bench child), ui-bundle-startup (gable-harness startup, path-patched), winprof allocator attribution, and a counting-allocator tool for front-end allocation counts and peak heap.
2. Equivalence oracle before refactoring: dump every test262 file (and the Gable bench bundles) compiled as script and module, recording the error message and span or a hash of the code tree's Debug print and serialised bytes; diff after each slice. Lexer fast-path unit tests.
3. Lexer: borrow unescaped identifier names from the source (Cow), scan decimal literals and escape-free strings without intermediate Strings, no Vec per escape.
4. Parser: intern names in one set with a fast hasher; fast hasher for scope maps.
5. Compiler: fast hasher for analysis and compiler maps; cut per-node allocations the profile shows (emit, bound_names, resolve, static_name).
6. Check the release profile (LTO/codegen-units) on compile-source.
7. Re-measure with alternated binaries; test262 (both modes, intl402), RegExp suites, parser tests, gc-stress subset, Gable suites.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Before/after numbers (same machine, before and after binaries run alternately; i9-9900KF, Windows 11):

| benchmark | before | after | speedup |
|---|---:|---:|---:|
| compile-source (bench child, 4 alternated rounds x 30 iterations) | 19.67 ms | 11.96 ms | 1.645x |
| ui-bundle-startup (gable-harness startup, 4 alternated rounds x 10 iterations) | 136.39 ms | 99.24 ms | 1.374x |
| ui-bundle-startup peak RSS | 43.6 MiB | 41.7 MiB | |
| ui-bundle-startup (run-bench.mjs, 2 alternated rounds) | 137.54 ms | 100.95 ms | 1.362x |

Front-end allocation counts (a counting global allocator; parse, and parse plus compile):
- generated.js (156 KB, the compile-source input): parse went from 138 079 to 33 046 allocations, and parse plus compile from 179 284 to 65 576. Peak memory went from 5 164 to 5 131 KB.
- @gable/ui bundle (1.39 MB): parse went from 576 253 to 179 125 allocations, and parse plus compile from 866 202 to 378 655. Peak memory went from 29 747 to 28 452 KB.

Root cause: most of the allocation was in the lexer, not the AST.
- Every identifier token was an owned String, re-interned as an `Rc<str>`.
- Every numeric literal was collected into a String before it was parsed.
- Every string literal and escape went through intermediate Vecs.
- The parser's and compiler's maps used SipHash.
- The compiler grew new instruction, offset and statement Vecs for every function, and built an owned String for each static property key.

Changes (three commits):
1. Lexer:
   - `Token<'s>` borrows from the source. An identifier name is a `Cow<'s, str>` that is owned only when it contains escapes.
   - Decimal literals are scanned in place; `_` separators are stripped only when present.
   - An ASCII string with no escapes is widened in one exact allocation.
   - Escapes produce inline `u16` units, with no Vec.
2. Parser and compiler: names are interned in one set, and the parser, analysis and compiler maps use foldhash. foldhash was already in the lockfile through hashbrown.
3. Compiler:
   - The emission buffers are pooled across functions and copied out at exact length.
   - `static_key_str` borrows static property keys for object literals, class fields and object patterns.

Bytecode equivalence oracle (out of tree):
- Every test262 test and harness file, plus the Gable bench bundles, is compiled as both script and module: 54 132 files.
- For each file it records the error message and span, or a hash of the CodeBlock tree's Debug print and of `serialize::encode`.
- The dump showed 0 differences after each slice, so the bytecode and the error messages and positions are identical.
- A lexer unit test (`fast_and_slow_paths_agree`) covers the fast and slow paths.

The bytecode format is unchanged, so ADR 0010 needs no version bump. No VM or JIT-relevant shape changed (ADR 0019).

Release profile: a fat-LTO, one-codegen-unit bench build is now only 1.02x on compile-source (11.79 against 12.03 ms) and 1.015x on bytecode-load. The 15% it used to give was mostly allocator inlining, which is gone, so the profile is unchanged.

Tests:
- test262 `--engine agnijs`: 42225/42225, with 0 regressions.
- test262 `--precompile`: 42225/42225.
- test262 `--only-intl402`: 981/981.
- RegExp suites (built-ins/RegExp, String.prototype, literals/regexp, annexB RegExp) under both `backtracking-only` and `linear-when-eligible`: 3222/3222.
- test262 `--engine parse`: 42495/42495.
- `cargo test --workspace --release`: all passed.
- gc-stress subset (`AGNIJS_GC_STRESS=1`, the vm tests gc and calls): passed.
- Gable suites via the path-patched harness: 759 passed, 1 failed. The failure is the known yjs editor timing test.

Other benchmarks:
- `bench run --engine agnijs`, two alternated pairs, then `bench compare`: no workload is slower by more than 5% in both pairs. The largest single-pair result is 1.04, and the other pair is 0.98 or 0.99.
- Gable run-bench (rxjs, signals, template and list-diff), 2 alternated rounds: all within 0.981x to 1.010x.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Front end allocation is cut, mostly in the lexer: tokens borrow from the source; decimals and plain strings are scanned in place; escapes no longer allocate. Names are interned once, foldhash replaces SipHash in the parser and compiler, and the compiler pools its emission buffers and borrows static keys. compile-source is 1.65x faster (19.7 to 12.0 ms) and ui-bundle-startup 1.37x (136.4 to 99.2 ms). Bytecode is unchanged: a 54 132-file equivalence dump shows 0 differences. test262 passes 42225/42225 in both modes, intl402 981/981 and the RegExp suites; the Gable suites pass 759 with only the known yjs timing failure; no other benchmark regressed more than 5%. Fat LTO is now worth about 2%, so the release profile is unchanged.
<!-- SECTION:FINAL_SUMMARY:END -->
