---
id: TASK-108
title: 'Regress: /(?:(_|)+){2}K/ on "__" never returns'
status: Done
assignee: []
created_date: '2026-11-09T05:08:00Z'
updated_date: '2026-11-09T21:10:00Z'
labels:
  - needs-triage
dependencies:
  - TASK-102
references:
  - docs/adr/0013-regexp-matching-adopts-regress-behind-a-seam.md
  - crates/agnijs-js-regex/src
type: bug
ordinal: 269000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The pattern /(?:(_|)+){2}K/ on the input "__" never returns from the backtracking matcher: an empty-matching alternative inside a quantified group inside another quantified group loops without making progress. The differential fuzzer limits quantifier nesting to two and keeps a second-level quantifier on single-character atoms to stay clear of it. Fixable upstream in regress or avoided by routing such patterns to the linear matcher, which finishes in linear time on the same input. Found by the differential fuzzer of TASK-102 (ADR 0013 addendum, "Divergences found in regress"). The linear matcher is not affected; affected patterns are routed to the backtracking matcher (regress) so both policies agree and behaviour is unchanged. This ticket removes that routing exception by fixing the divergence, either upstream in the regress crate (preferred: report it, send a patch, then bump the dependency) or in the seam or the linear matcher if upstream is unresponsive.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 The pattern terminates on "__" with the result required by the spec (ECMAScript empty-check semantics for quantified groups), on both matchers, with a regression test that runs under a timeout
- [x] #2 Either regress is fixed upstream and bumped, or the routing sends nested-quantifier patterns that can match empty to the linear matcher under Auto and BacktrackingOnly documents the exception; the fuzzer generator limits noted in tests/differential.rs are lifted or re-justified
- [x] #3 test262 RegExp suites and baselines are unchanged
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Fixed by TASK-100.31: the backtracking matcher fails an iteration past the minimum that consumes nothing (RepeatMatcher empty check), so every loop terminates; /(?:(_|)+){2}K/ returns null on __ and matches __K. tests/regress_bugs.rs runs it in every mode on a thread with a 10 s timeout. AC2: regress is no longer the matcher, so no routing exception is needed; matchers_agree now nests quantifiers three deep, and the regress comparison keeps two levels because regress itself hangs, as tests/differential.rs documents. test262 RegExp 2502/2502 in all three modes.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Fixed in our own backtracking matcher (TASK-100.31) with the ECMAScript empty-iteration check, so the pattern terminates with the spec result on both matchers and in every mode. Regression test under a timeout in tests/regress_bugs.rs, VM-level test in tests/regexp.rs. The fuzzer limit is lifted for our matchers and re-justified for the regress reference; test262 unchanged.
<!-- SECTION:FINAL_SUMMARY:END -->
