---
id: TASK-100.29
title: >-
  Engine: less allocation in regex exec and replace, string case mapping and
  split, and JSON
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
- [ ] #1 regex and string-methods are at least 1.3x faster, and json-parse and json-stringify at least 1.15x faster, than at the start of the task
- [ ] #2 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [ ] #3 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->
