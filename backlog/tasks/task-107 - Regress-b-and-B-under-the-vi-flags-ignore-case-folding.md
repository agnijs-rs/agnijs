---
id: TASK-107
title: 'Regress: \b and \B under the vi flags ignore case folding'
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
ordinal: 268000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Under the v and i flags together, \b and \B do not use the case-folded word characters that ECMAScript requires (for example U+017F and U+212A count as word characters when matching with u or v and i). The linear matcher deliberately matches regress here so the two agree; both are wrong. Found by the differential fuzzer of TASK-102 (ADR 0013 addendum, "Divergences found in regress"). The linear matcher is not affected; affected patterns are routed to the backtracking matcher (regress) so both policies agree and behaviour is unchanged. This ticket removes that routing exception by fixing the divergence, either upstream in the regress crate (preferred: report it, send a patch, then bump the dependency) or in the seam or the linear matcher if upstream is unresponsive.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 \b and \B use the case-folded word-character set under (u or v) and i, per the RegExp Matcher semantics (WordCharacters), on both matchers, with a regression test
- [x] #2 The linear matcher word test is changed together with the backtracking one so the differential fuzzer keeps zero mismatches, and test262 RegExp baselines are unchanged or improved
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Fixed by TASK-100.31: under i with u or v, \b and \B test against the word characters closed under simple case folding (U+017F and U+212A included). Both matchers use the same set from the parse tree. tests/regress_bugs.rs covers ui and vi, plus the u-only control, in every mode; the VM regexp test covers it from JavaScript. test262 RegExp 2502/2502 in all three modes, unchanged.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Fixed in our own regex engine (TASK-100.31): \b and \B use the case-folded WordCharacters under (u or v) and i, the same set in the backtracking and linear matchers, so the matchers agree (differential fuzzing at 200000 cases, zero mismatches). Regression tests in tests/regress_bugs.rs and the VM regexp tests; test262 baselines unchanged.
<!-- SECTION:FINAL_SUMMARY:END -->
