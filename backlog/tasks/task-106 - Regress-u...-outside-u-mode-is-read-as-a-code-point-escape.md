---
id: TASK-106
title: 'Regress: \u{...} outside u mode is read as a code-point escape'
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
ordinal: 267000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Outside u mode, \u{2} must be the letter u repeated twice (Annex B identity escape followed by a quantifier {2}), but regress reads it as a code-point escape. Found by the differential fuzzer of TASK-102 (ADR 0013 addendum, "Divergences found in regress"). The linear matcher is not affected; affected patterns are routed to the backtracking matcher (regress) so both policies agree and behaviour is unchanged. This ticket removes that routing exception by fixing the divergence, either upstream in the regress crate (preferred: report it, send a patch, then bump the dependency) or in the seam or the linear matcher if upstream is unresponsive.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 /\u{2}/ matches "uu" and not "\u0002" outside u mode, and braced \u{...} stays a code-point escape in u and v modes, on both matchers
- [x] #2 A regression test covers it and the linear-eligibility routing exception for braced \u{} outside u mode is removed, with the differential fuzzer still showing zero mismatches
- [x] #3 If the fix is upstream, the regress dependency is bumped (or patched with a recorded reason) and test262 baselines are unchanged
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Fixed by TASK-100.31: outside u mode \u{2} parses as an identity escape u with the quantifier {2}; in u and v mode it is a code-point escape. tests/regress_bugs.rs checks both readings in every MatcherMode and linear eligibility; agnijs-js-vm tests/regexp.rs checks it under every host policy. AC3: no upstream fix, regress is now a test reference only; test262 unchanged (42231/42231, RegExp 2502/2502 in all three modes).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Fixed in our own regex engine (TASK-100.31): /\u{2}/ outside u mode matches uu and not U+0002, braced escapes stay code points in u and v modes, on both matchers and in every mode. The linear-eligibility exception is removed. Regression tests in tests/regress_bugs.rs and the VM regexp tests; differential fuzzing shows zero mismatches.
<!-- SECTION:FINAL_SUMMARY:END -->
