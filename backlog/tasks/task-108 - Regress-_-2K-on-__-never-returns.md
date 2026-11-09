---
id: TASK-108
title: 'Regress: /(?:(_|)+){2}K/ on "__" never returns'
status: To Do
assignee: []
created_date: '2026-11-09T05:08:00Z'
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
- [ ] #1 The pattern terminates on "__" with the result required by the spec (ECMAScript empty-check semantics for quantified groups), on both matchers, with a regression test that runs under a timeout
- [ ] #2 Either regress is fixed upstream and bumped, or the routing sends nested-quantifier patterns that can match empty to the linear matcher under Auto and BacktrackingOnly documents the exception; the fuzzer generator limits noted in tests/differential.rs are lifted or re-justified
- [ ] #3 test262 RegExp suites and baselines are unchanged
<!-- AC:END -->
