---
id: TASK-100.30
title: 'Regex: fail an anchored pattern without scanning the whole input'
status: To Do
assignee: []
created_date: '2026-11-09T03:19:00Z'
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
- [ ] #1 When every alternative of a pattern without the m flag starts with ^, exec and test try only the start position, and fail at once when lastIndex is past 0 for a global or sticky regex
- [ ] #2 The reproduction at N = 4e6 runs in under 10 ms, with tests for alternatives, groups, lookbehind and the m flag where the shortcut must not apply
- [ ] #3 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [ ] #4 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->
