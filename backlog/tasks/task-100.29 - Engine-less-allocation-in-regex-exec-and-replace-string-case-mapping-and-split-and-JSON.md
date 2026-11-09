---
id: TASK-100.29
title: >-
  Engine: less allocation in regex exec and replace, string case mapping and
  split, and JSON
status: Done
assignee:
  - '@claude'
created_date: '2026-11-09T03:19:00Z'
updated_date: '2026-11-09T19:01:00Z'
labels:
  - ready-for-agent
dependencies: []
documentation:
  - docs/js-engine-performance-findings.md
parent_task_id: TASK-100
priority: low
type: enhancement
ordinal: 263000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The built-in string, regex and JSON paths allocate heavily. regex: exec and replace allocate the match array, capture strings and UTF-16 copies (RtlAllocateHeap 22.8% self), while regress's own matching is about 2%; the micro-benchmark is 2.56x. string-methods (1.71x): split and toLowerCase map case through a char iterator and re-encode with alloc_str_utf16. json-stringify (1.22x): number formatting (grisu plus to_js_string) is 22%. json-parse (1.53x): Vec growth without reserving.

Reproduction: profile regex, string-methods, json-stringify and json-parse with winprof and read the allocator share per built-in.

Benchmarks to prove it: regex, string-methods, json-parse, json-stringify (micro).

Findings: docs/js-engine-performance-findings.md, finding 9f. Keep the engine crates Gable-agnostic (ADR 0016); the agnijs-* rename is TASK-100.12, so use the current crate names. Measure before and after with the same binaries and machine; micro-benchmarks via `native/engine/target/release/bench.exe run --engine gable --engine quickjs` and `bench compare`, Gable benchmarks via `node native/engine-harness/js/run-bench.mjs --engine gable --engine quickjs`. The sampling profiler is native/tools/winprof (see the findings doc, Method).

Model assignment (maintainer, 2026-11-08, via TASK-100.14): Fable.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 regex and string-methods are at least 1.3x faster, and json-parse and json-stringify at least 1.15x faster, than at the start of the task
- [x] #2 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [x] #3 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Root cause (winprof): RtlAllocateHeap plus RtlFreeHeap were 30 to 43% self time in the four benches. The built-ins copied strings to UTF-16, built intermediate Vecs, used format!, and materialised a full exec result array for every match inside @@replace, @@match and @@split.

Changes:
- `StrBuilder` (one-byte or two-byte, widens on demand) and `Heap::substring`. Slices, split parts, trims and captures are substrings.
- String search compares `Units` in place, so it no longer copies.
- Case mapping has an ASCII and a Latin-1 path, and returns the receiver when nothing changes.
- `GetSubstitution` expands straight into the output builder.
- The regex cache is a hashbrown table keyed by the pattern's units, so a hit allocates nothing.
- Two-byte input is matched in place, short one-byte input is widened into a reused scratch buffer, and long one-byte input goes through a small cache.
- The built-in exec is tagged `NativeTag::RegExpExec`. When `exec` is unmodified, the symbol methods use the raw capture ranges and skip the result array. User exec keeps the spec Get order.
- JSON parses the source in its own width with no copy, stringifies into one builder, and formats numbers into a stack buffer (`format_js_number`).
- Fixed a pre-existing panic: `/(?:)/gu` with lastIndex inside a surrogate pair sliced backwards.

Allocations per call (crates/agnijs-js-bench/tests/builtin_allocs.rs, before -> after):

| Call | Before | After |
| --- | ---: | ---: |
| exec, two captures | 13 | 7 |
| exec, no match | 4 | 2 |
| test | 8 | 3 |
| global replace, 5 matches | 85 | 22 |
| replace with $1 template | 36 | 12 |
| global match | 63 | 26 |
| split by regex, 3 parts | 254 | 155 |
| split(' '), 20 parts | 46 | 24 |
| toLowerCase / toUpperCase | 3 | 1 |
| slice | 2 | 1 |
| indexOf | 1 | 0 |
| replace(str, str) | 6 | 2 |
| trim | 2 | 1 |
| JSON.parse, 9 values | 34 | 8 |
| JSON.stringify, 9 values | 50 | 9 |
| JSON.stringify of a number | 5.47 | 2 |
| Object.keys | 3 | 2 |
| Object.entries | 9 | 8 |
| join | 8 | 8 |
| Map/Set | 0 | 0 |

Micro benches (bench run, 10 iterations, 2 warmup), base (2f18f7c) and new alternated over 3 rounds, median ms:

| Bench | Base | New | Speedup |
| --- | ---: | ---: | ---: |
| regex | 291.32 | 155.92 | 1.87x |
| string-methods | 56.49 | 32.14 | 1.76x |
| json-parse | 113.34 | 44.49 | 2.55x |
| json-stringify | 175.73 | 47.03 | 3.74x |
| string-concat | 70.19 | 64.51 | 1.09x |
| map-set | 130.32 | 123.01 | 1.06x |
| regex-long-input | 7.52 | 7.27 | 1.03x |
| regex-tokenize | 45.86 | 47.39 | 0.97x |

Every other micro bench is between 0.98x and 1.02x (loop-arith 0.99, array-methods 0.99, promise-chain 0.99, async-await 0.99, engine-startup 0.98). regex-tokenize was 0.97x in two separate sweeps. That is a consistent 3%, inside the 5% bar.

Gable benches (gable-harness built against each checkout by path, alternated over 3 rounds, median ms, base -> new):

| Bench | Base | New | Speedup |
| --- | ---: | ---: | ---: |
| rxjs-operators | 175.00 | 180.64 | 0.97x |
| rxjs-higher-order | 281.16 | 279.52 | 1.01x |
| rxjs-subject-fanout | 122.52 | 125.56 | 0.98x |
| signal-churn | 460.03 | 466.82 | 0.99x |
| signal-create | 87.46 | 86.51 | 1.01x |
| template-mount | 66.42 | 66.23 | 1.00x |
| template-update | 38.89 | 39.10 | 0.99x |
| list-diff | 783.57 | 788.00 | 0.99x |
| ui-bundle-startup | 100.31 | 95.83 | 1.05x |

No bench regresses by more than 5%.

Verification:
- New tests:
  - crates/agnijs-js-bench/tests/builtin_equivalence.rs compares AgniJS with QuickJS-NG, normally and with a collection at every safe point, on edge cases of JSON, string search/split/replace/case, RegExp exec/test/lastIndex, @@replace/@@match/@@split, and exec mutated during the call.
  - builtin_allocs.rs has per-call allocation bounds.
- cargo test --release on the vm, parser, regex and bench crates is green.
- AGNIJS_GC_STRESS=1 on ic, calls, gc, shapes, dispatch, gc_safe_points, alloc_weight and promises is green.
- AGNIJS_GC_STRESS=16 test262 slices:
  - String, JSON and the RegExp subdirectories: 1922/1922.
  - The remaining RegExp files: 751/751. property-escapes and CharacterClassEscapes were left out, because they did not finish in 40 minutes under stress; they pass in the normal runs.
  - The language slice (class, assignment, for-of, template-literal, regexp and string literals): 6315/6315.
- test262 agnijs 42225/42225 and --precompile 42225/42225.
- intl402 981/981.
- RegExp and String regex slices 2252/2252 under both backtracking-only and linear-when-eligible.
- Gable suites via the path-patched harness: 758 passed, 2 known timing failures (yjs perf, highlighter). The harness manifest and lockfile were restored afterwards.

Follow-ups (not done):
- Split by regex still costs about 155 allocations for 3 parts, because regress sets up again at every position; a scan that finds the next match from the current position instead of a sticky match at each one would remove it.
- regex-tokenize is 3% slower, probably from the input-cache lookup on long one-byte input for each sticky exec.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Cut allocation in the string, RegExp and JSON built-ins: a one/two-byte StrBuilder and substrings instead of UTF-16 copies, in-place search and Latin-1 case mapping, an allocation-free regex cache hit, raw exec captures for @@replace/@@match/@@split/@@search when exec is the built-in, and JSON that parses in the source width and stringifies into one builder. regex 1.87x, string-methods 1.76x, json-parse 2.55x, json-stringify 3.74x; no other micro or Gable bench is more than about 3% slower (regex-tokenize and rxjs-operators 0.97x). test262 42225/42225 (default and --precompile), intl402 981/981, RegExp slices under both matchers, GC-stress slices and Gable suites (758, 2 known timing failures) pass. New equivalence tests against QuickJS-NG (also under GC stress) and per-call allocation bounds. Follow-ups: split by regex still re-runs regress setup per position; regex-tokenize is 3% slower.
<!-- SECTION:FINAL_SUMMARY:END -->
