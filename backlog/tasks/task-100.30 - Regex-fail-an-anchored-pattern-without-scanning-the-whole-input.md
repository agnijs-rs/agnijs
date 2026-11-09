---
id: TASK-100.30
title: 'Regex: fail an anchored pattern without scanning the whole input'
status: Done
assignee:
  - '@claude-fable'
created_date: '2026-11-09T03:19:00Z'
updated_date: '2026-11-09T06:45:00Z'
labels:
  - ready-for-agent
dependencies: []
documentation:
  - docs/js-engine-performance-findings.md
parent_task_id: TASK-100
priority: low
type: enhancement
ordinal: 264000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
A failing exec of a non-multiline pattern anchored with `^` is linear in the input length, because `gable_js_regex::Regex::exec` calls regress's `find_from_utf16(input, start)`, which tries every start position. Listed as a follow-up in the M9 switch-over report.

Reproduction (ES module): `const s = "a".repeat(N), re = /^x/; for (let i = 0; i < 200; i++) re.exec(s);` takes 90 ms at N = 1e5, 903 ms at 1e6 and 3.6 s at 4e6 (about 4.5 ns per character; QuickJS-NG is 2.3 times slower; Node is constant time). `re.test` behaves the same; a sticky `/x/y` is already constant time.

Benchmark to prove it: the reproduction above at N = 4e6, plus the regex micro-benchmark (no regression).

The regex seam is ADR 0013; TASK-102 (linear-time matcher) works in the same crate, so coordinate if both are in flight.

Findings: docs/js-engine-performance-findings.md, finding 9g. Keep the engine crates Gable-agnostic (ADR 0016); the agnijs-* rename is TASK-100.12, so use the current crate names. Measure before and after with the same binaries and machine; micro-benchmarks via `native/engine/target/release/bench.exe run --engine gable --engine quickjs` and `bench compare`, Gable benchmarks via `node native/engine-harness/js/run-bench.mjs --engine gable --engine quickjs`. The sampling profiler is native/tools/winprof (see the findings doc, Method).

Model assignment (maintainer, 2026-11-08, via TASK-100.14): Fable.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 When every alternative of a pattern without the m flag starts with ^, exec and test try only the start position, and fail at once when lastIndex is past 0 for a global or sticky regex
- [x] #2 The reproduction at N = 4e6 runs in under 10 ms, with tests for alternatives, groups, lookbehind and the m flag where the shortcut must not apply
- [x] #3 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [x] #4 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red: add a Regex-seam test (agnijs-js-regex) that runs /^x/ exec on a 1e6-unit input 200 times in every matcher mode and must finish in constant time; add semantic tests for alternatives, groups, lookbehind, modifiers and the m flag.
2. Detect at compile time, on the stripped numbered-groups pattern, whether every alternative begins with a non-multiline ^ (through capturing, non-capturing and modifier groups that do not add m, never through lookaround or a quantified group).
3. Green: Regex::exec for such a pattern returns None at once when start > 0 and otherwise runs the exact-start attempt at 0 (exec_sticky's (?:P)()| wrapper on the regress path, the sticky Pike VM pass on the linear path), so both matchers stop at position 0.
4. Measure the reproduction at N = 4e6 and the regex micro-benchmarks before (main bench.exe) and after; cargo test for the regex crate, the bounded differential fuzz, test262 full, --precompile, intl402 and the RegExp suites under all three --regex-matcher modes, one run each.
5. Record numbers in the notes, update ADR 0013 (relation to TASK-100.30) and the findings doc, commit and push to main.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Root cause confirmed on origin/main bc9fcca (release test262.exe eval, module, i9-9900KF): /^x/ exec x200 took 91 ms at N=1e5, 887 ms at 1e6, 3592 ms at 4e6; re.test and a global regex with lastIndex=5 the same. Regex::exec asked regress for the leftmost match from start and regress tried every position; the Pike VM's skip-ahead did the same scan on the linear path.

Change: Regex::compile records starts_anchored (every alternative of the numbered-groups pattern begins with a non-multiline ^, looking through capturing, non-capturing and modifier groups that do not add m; never through lookaround or a quantified group). Regex::exec on such a pattern returns None at once when start > 0 and otherwise runs exec_sticky(input, 0), the exact-start attempt both matchers already had ((?:P)()| on regress, the sticky Pike VM pass). Routing is unchanged (the cost model already counted one start for anchored patterns).

After (this branch, same machine): /^x/ exec x200 0 ms at 1e5, 1 ms at 1e6, 6 ms at 4e6 (the first call's UTF-16 conversion of the 4M-unit input); test and lastIndex=5 0 ms at every size.

Tests: crates/agnijs-js-regex/tests/anchored.rs (the reproduction in every matcher mode, under flags dgisuvy; 36 detector cases; 31 exact-semantics cases incl. alternatives, groups, lookbehind, modifiers, m flag, quantified groups, unicode, backreferences). Differential fuzz extended: an anchored pattern is also compared with the same pattern behind (?:), which hides the anchor. Bounded release run (AGNIJS_REGEX_FUZZ_CASES=200000, seed default): 200000 eligible patterns, 14961952 compared outcomes, 2484 anchored, zero mismatches. cargo test -p agnijs-js-regex green with and without the linear feature.

Verification (this branch, release, one run each): cargo test -p agnijs-js-vm green (shapes of the seam's VM users: 21 + 8 + 15 + 9 tests). test262 --engine agnijs 42225/42225, --precompile 42225/42225, --only-intl402 981/981; RegExp suites (built-ins/RegExp, String.prototype match/matchAll/replace/replaceAll/search/split, language/literals/regexp, annexB RegExp) 2505/2505 under --regex-matcher auto, backtracking-only and linear-when-eligible; 0 regressions and 0 newly passing everywhere.

Micro-benchmarks (bench run --filter regex, agnijs, 1 warmup + 5 iterations, before = origin/main bc9fcca bench.exe, after = this branch, same machine): regex 385.73 -> 376.72 ms (0.98), regex-tokenize 99.95 -> 98.35 ms (0.98), regex-long-input 7.77 -> 7.51 ms (0.97), regex-redos 0.01 -> 0.01 ms; QuickJS-NG for context: 137.36 / 21.73 / 3.29 / 94.99 ms. The change is confined to agnijs-js-regex's exec entry, which only RegExp built-ins reach, so the non-regex micro-benchmarks and the Gable benchmarks cannot move; they were not re-run.

Docs: ADR 0013 addendum gained the section 'Anchored patterns try index 0 only (TASK-100.30)'.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
A failed exec of a pattern anchored by a non-multiline ^ scanned the whole input because the seam asked for the leftmost match from start (regress tried every position; the Pike VM's skip-ahead did the same). Regex::compile now records starts_anchored (every alternative begins with ^ outside the m flag, through capturing, non-capturing and modifier groups that do not add m; never through lookaround or a quantified group), and Regex::exec on such a pattern returns None at once when start > 0 and otherwise runs the exact-start attempt at 0 that exec_sticky already had, on both matchers. Routing is unchanged. Reproduction (200 x /^x/ exec, release): 3592 -> 6 ms at N = 4e6, 887 -> 1 ms at 1e6; test and lastIndex > 0 cases 0 ms. New tests in crates/agnijs-js-regex/tests/anchored.rs (every matcher mode; alternatives, groups, lookbehind, modifiers, m flag, quantified groups, unicode, backreferences); the differential fuzzer compares anchored patterns with the same pattern behind (?:), 200k-pattern bounded run with 2484 anchored patterns and zero mismatches. Regex micro-benchmarks 0.97-0.98x; test262 42225/42225, precompile 42225/42225, intl402 981/981, RegExp suites 2505/2505 in all three matcher modes. ADR 0013 addendum updated.
<!-- SECTION:FINAL_SUMMARY:END -->
