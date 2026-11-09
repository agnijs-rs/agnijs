---
id: TASK-100.34
title: 'Regex: deeply nested groups must not overflow the stack'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-09T21:12:00Z'
updated_date: '2026-11-09T21:36:00Z'
labels:
  - needs-triage
dependencies: []
parent_task_id: TASK-100
ordinal: 283000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The regex pattern validator and the tree compiler in agnijs-js-regex recurse on group nesting. On a 2 MB stack they overflow at about 2,100 and 1,500 nested groups, so a deep regex literal or new RegExp(...) call on untrusted input can crash the process instead of throwing a SyntaxError (found during TASK-100.31, see the ADR 0013 addendum). Add a nesting depth limit or make both passes iterative, with tests for deeply nested groups, classes and lookarounds, and cover it in the regex fuzz targets. Model: Opus.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A pattern nested far past the limit throws SyntaxError (or matches correctly) and never overflows the stack, on the default 2 MB thread stack
- [x] #2 Regression tests cover deep groups, lookarounds and v-mode classes, and differential fuzzing includes deep-nesting cases
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Tests first (tests/nesting.rs, VM tests): every kind of nesting (groups, non-capturing, quantified, alternation, modifiers, lookarounds, v-mode nested classes and set operations) at the limit, one past it and 100,000 past it, via syntax::validate, Regex::compile in all three modes, program bytes, regex literals, new RegExp and precompiled bytecode, on an explicit 2 MB stack.
2. Measure the stack each level costs in every pass (debug ~4.8 KB, release ~1.3 KB on the worst kinds).
3. Add syntax::MAX_NESTING (256): groups, lookarounds and classes counted together in the validator, which runs before the parser, compilers and analyses; deeper is a SyntaxError. Rejected rewriting the ~10 recursive walks iteratively (cost vs a limit no real pattern reaches). Programs from bytes are already flat and verified with explicit stacks.
4. Deep-nesting differential fuzz test.
5. ADR 0013 addendum section; full verification (test262 default/precompile/intl402/RegExp x3 modes, benches, gc-stress subset, Gable suites).
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Measured before the fix (debug, 2 MB thread): groups overflowed the validator at ~544 levels and parse+compile at ~430-530; lookarounds ~850; v-mode classes ~416. Release: ~1,570 for every kind (~1.3 KB/level). Limit 256 leaves the worst pass at ~1.3 MB debug / ~340 KB release.
Verification: tests/nesting.rs 7/7 (each kind at the limit in three modes plus bytes round trip, one past, 100,000 past, unbalanced, combined kinds, a 100,000-deep lookaround program from bytes); VM tests (literal early SyntaxError, new RegExp SyntaxError for groups/lookbehind/lookahead-u/v classes, limit-deep literal from source and precompiled bytecode); differential deep-nesting fuzz 1,000 patterns in cargo test and 20,000 in a bounded release run (3,850 eligible, 5,316 too deep), no mismatch. Regex, parser, compiler and VM crate tests pass.
test262: 42231/42231 default and --precompile; intl402 981/981; RegExp filter set 2511/2511 (the CI filter set, now counted at 2511) under auto, backtracking-only and linear-when-eligible; gc-stress (AGNIJS_GC_STRESS=16) on literals/regexp, RegExp/prototype/exec and statements/class 4722/4722.
Benches (release, alternating, median of 3 runs x7): regex 103.8 -> 102.4 ms, regex-tokenize 32.5 -> 31.3 ms, regex-long-input 1.59 -> 1.62 ms (noise).
Gable suites via path-patched harness: 758 passed, 2 known timing failures (yjs perf, highlight); manifest and lockfile restored.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Deeply nested regex patterns no longer overflow the stack. syntax::MAX_NESTING (256) bounds groups, lookarounds and character classes, counted together, in the validator, which runs before the parser, both compilers and the tree analyses for literals, new RegExp and precompilation; deeper patterns are a SyntaxError ('regular expression nested too deeply'). Making the ~10 recursive walks iterative was rejected as far costlier than a limit no real pattern reaches; programs loaded from bytes were already flat and verified iteratively, and a test now pins that (100,000 nested lookarounds load and run). Tests: tests/nesting.rs, VM regexp tests, and a deep-nesting differential fuzz test. ADR 0013 addendum's known-limit section replaced by the nesting-limit section. No test262, bench or Gable suite regressions.
<!-- SECTION:FINAL_SUMMARY:END -->
