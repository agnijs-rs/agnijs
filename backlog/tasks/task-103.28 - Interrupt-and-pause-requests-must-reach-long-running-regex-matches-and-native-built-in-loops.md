---
id: TASK-103.28
title: >-
  Interrupt and pause requests must reach long-running regex matches and native
  built-in loops
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-12T06:25:00Z'
updated_date: '2026-11-12T15:30:00Z'
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
- [ ] #1 The backtracking matcher checks for an interrupt at a bounded step interval, and an interrupted match fails the running call with Error::Interrupted (AGNIJS_INTERRUPTED in the C API) without changing match results
- [ ] #2 Native built-ins whose work grows with their input poll the interrupt at a bounded interval
- [ ] #3 Regression tests interrupt an exponential lookahead pattern under Auto, a nested-quantifier pattern under BacktrackingOnly and a join over a multi-million-element array, each within a bounded time
- [ ] #4 The fuel documentation states what fuel does and does not count inside native calls after the change
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red tests (crates/agnijs-js-vm/tests/interrupt_native.rs): a host whose interrupt hook always says stop; exponential lookahead under Auto, nested quantifier under BacktrackingOnly, Array(3e6).join, sparse array-like loops, quadratic string search; each must fail with Error::Interrupted within a bounded time and call the hook. Plus a match-results-unchanged check under a never-stop hook.
2. agnijs-js-regex: a polled search API (Regex::exec_polled / exec_sticky_polled returning Result<Option<Captures>, Interrupted>). The backtracker's step counter runs in slices; at each slice end a cold path asks the poll callback and refills from the remaining Auto budget, so the hot loop keeps one counter. The linear matcher polls per chunk of thread-steps. exec/exec_sticky stay as never-interrupted wrappers.
3. VM: builtin_exec_raw passes a poll that asks Host::interrupt directly (Fault::Interrupt on stop). Native loops (Array, TypedArray, String search/split/replace, RegExp match/replace/split, JSON parse/stringify, collections, Object loops, iterator helpers) call vm.poll_interrupt() per iteration; string search gains a polled variant weighted by needle length.
4. C API test: agnijs_engine_request_interrupt stops a backtracking regex. Docs: security-model.md, fuel docs (Engine::set_fuel), Host::regex_matcher / RegexMatcher docs, ADR 0013 note.
5. Gates: clippy, rustfmt own hunks, test262 x4 + QuickJS-NG baseline, GC-stress slice, JIT slices if needed, regex-focused benchmarks within 1%.
<!-- SECTION:PLAN:END -->
