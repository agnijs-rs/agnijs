---
id: TASK-103.28
title: >-
  Interrupt and pause requests must reach long-running regex matches and native
  built-in loops
status: Done
assignee:
  - '@claude'
created_date: '2026-11-12T06:25:00Z'
updated_date: '2026-11-12T20:05:00Z'
labels:
  - ready-for-agent
  - security
dependencies: []
references:
  - docs/security-model.md
parent_task_id: TASK-103
priority: high
ordinal: 292000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the security review (TASK-103.9, docs/security-model.md). `Vm::poll_interrupt` (`vm.rs`) runs only at function entry, loop back-edges and call returns in the interpreter and the JIT; the regex matchers (`agnijs-js-regex`) and native built-ins that loop over their input (Array join, sort, fill, indexOf, includes and copyWithin on huge array-likes, TypedArray methods, String methods, JSON) never poll it. Patterns the linear-time matcher cannot run (backreferences, lookaround), and every pattern under `RegexMatcher::BacktrackingOnly`, backtrack with no step budget (ADR 0013 addendum). A probe with an interrupt hook that always returned true ran `/(?=(a+)+b)/.test('a'.repeat(23))` under the default `Auto` policy and `/(a+)+b/` under `BacktrackingOnly` to completion (3.9 s each in a debug build, doubling with each extra character), and `Array(3000000).join(',')` (0.7 s); the hook was called zero times. Fuel does not help either: a native call costs one instruction. The `Host::regex_matcher` docs (`agnijs-engine-api/src/host.rs`) and ADR 0013 say the host interrupt is the guard for pathological patterns, which is not true today. `agnijs_engine_request_interrupt` and the debugger's `PauseRequester` have the same blind spot.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 The backtracking matcher checks for an interrupt at a bounded step interval, and an interrupted match fails the running call with Error::Interrupted (AGNIJS_INTERRUPTED in the C API) without changing match results
- [x] #2 Native built-ins whose work grows with their input poll the interrupt at a bounded interval
- [x] #3 Regression tests interrupt an exponential lookahead pattern under Auto, a nested-quantifier pattern under BacktrackingOnly and a join over a multi-million-element array, each within a bounded time
- [x] #4 The fuel documentation states what fuel does and does not count inside native calls after the change
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red tests (crates/agnijs-js-vm/tests/interrupt_native.rs): a host whose interrupt hook always says stop; exponential lookahead under Auto, nested quantifier under BacktrackingOnly, Array(3e6).join, sparse array-like loops, quadratic string search; each must fail with Error::Interrupted within a bounded time and call the hook. Plus a match-results-unchanged check under a never-stop hook.
2. agnijs-js-regex: a polled search API (Regex::exec_polled / exec_sticky_polled returning Result<Option<Captures>, Interrupted>). The backtracker's step counter runs in slices; at each slice end a cold path asks the poll callback and refills from the remaining Auto budget, so the hot loop keeps one counter. The linear matcher polls per chunk of thread-steps. exec/exec_sticky stay as never-interrupted wrappers.
3. VM: builtin_exec_raw passes a poll that asks Host::interrupt directly (Fault::Interrupt on stop). Native loops (Array, TypedArray, String search/split/replace, RegExp match/replace/split, JSON parse/stringify, collections, Object loops, iterator helpers) call vm.poll_interrupt() per iteration; string search gains a polled variant weighted by needle length.
4. C API test: agnijs_engine_request_interrupt stops a backtracking regex. Docs: security-model.md, fuel docs (Engine::set_fuel), Host::regex_matcher / RegexMatcher docs, ADR 0013 note.
5. Gates: clippy, rustfmt own hunks, test262 x4 + QuickJS-NG baseline, GC-stress slice, JIT slices if needed, regex-focused benchmarks within 1%.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented in 8459ae4. Regex: Meter in backtrack.rs counts steps in 65,536-step slices and asks the poll at each slice end, refilling from the Auto budget (so the budget, matcher choice and results are unchanged); the linear matcher polls per 65,536 thread-steps; Regex::exec_polled / exec_sticky_polled return Err(Interrupted). VM: RegExp exec polls Host::interrupt directly (Fault::Interrupt); native loops call Vm::poll_interrupt per iteration; JSON.parse asks once per 4096 values through a parser-local counter (a per-element Vm::poll_interrupt cost json-parse 4%); string searches charge compared units and ask once per 2^20 (SearchPoll). Not polled, as bounded linear work on one string or buffer: byte copies and fills (repeat/pad builders, TypedArray reverse and same-buffer slice, which also hit borrow conflicts), case mapping, normalization, URI/base64/hex, the regex first-character scan. Pause requests (PauseRequester) are still only taken at statements: a pause lands after a long native call returns; only the interrupt stops it partway. Documented in docs/security-model.md; no AC covers it.

Gates: clippy and rustfmt clean on changed lines; workspace tests; regex and vm regexp/interrupt_native tests without default features; test262 0 regressions (default, --precompile, --snapshot, --only-intl402, QuickJS-NG baseline, RegExp suites under backtracking-only and linear-when-eligible); GC stress (vm+compiler units, capi subset, test262 language slice at 16); JIT forced VM suite, jit_workloads, test262 slice forced and tiered; header matches cbindgen. Benchmarks vs 60d8c31 (agnijs, 3 alternating rounds, min of medians): all 26 workloads -0.74% geomean, regex workloads -1.67%; json-parse +1.4% / json-stringify -2.2% after the parser-counter fix. CI on 8459ae4: all 23 jobs green.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Interrupt requests now reach long-running regex matches and native built-in loops. Both regex matchers ask the host every 65,536 steps without changing the Auto budget or match results; native loops over their input, JSON and string searches poll at bounded intervals; an interrupted call fails with Error::Interrupted / AGNIJS_INTERRUPTED. Regression tests (tests/interrupt_native.rs, regex unit tests, a C API cross-thread test) stop exponential lookahead under Auto, nested quantifiers under BacktrackingOnly, a 3,000,000-element join and more within 2 s. Fuel docs now say fuel counts bytecode only. All gates and CI green; benchmarks within 1%. Not done: debugger pause requests still wait for the native call to return (documented limitation; would need a separate task).
<!-- SECTION:FINAL_SUMMARY:END -->
