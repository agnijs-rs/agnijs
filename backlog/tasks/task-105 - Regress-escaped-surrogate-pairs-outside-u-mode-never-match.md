---
id: TASK-105
title: 'Regress: escaped surrogate pairs outside u mode never match'
status: Done
assignee: []
created_date: '2026-11-09T05:07:00Z'
updated_date: '2026-11-09T21:10:00Z'
labels:
  - needs-triage
dependencies:
  - TASK-102
references:
  - docs/adr/0013-regexp-matching-adopts-regress-behind-a-seam.md
  - crates/agnijs-js-regex/src
type: bug
ordinal: 266000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Outside u mode, /\uD83D\uDE00/ never matches the two code units it names, because regress joins the escapes into one code point. An escaped lead surrogate followed by another \u escape also misses: /\uD83D\u0062?/ does not find the lone lead in "a\uD83D\uDE00". Found by the differential fuzzer of TASK-102 (ADR 0013 addendum, "Divergences found in regress"). The linear matcher is not affected; affected patterns are routed to the backtracking matcher (regress) so both policies agree and behaviour is unchanged. This ticket removes that routing exception by fixing the divergence, either upstream in the regress crate (preferred: report it, send a patch, then bump the dependency) or in the seam or the linear matcher if upstream is unresponsive.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 /\uD83D\uDE00/ matches the string "\uD83D\uDE00" outside u mode, and /\uD83D\u0062?/ matches the lone lead in "a\uD83D\uDE00", on both matchers and under every MatcherMode
- [x] #2 A regression test covers both patterns (in agnijs-js-regex tests and the VM regexp tests), and the linear-eligibility routing exception for an escaped lead surrogate followed by another \u escape is removed, with the differential fuzzer still showing zero mismatches
- [x] #3 If the fix is upstream, the regress dependency is bumped (or patched with a recorded reason) and test262 RegExp suites and baselines are unchanged
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Fixed by TASK-100.31, which replaced regress with our own matchers. The parser keeps escaped surrogates as code units outside u mode. tests/regress_bugs.rs checks both patterns in every MatcherMode and that both are now linear-eligible; agnijs-js-vm tests/regexp.rs checks them from JavaScript under the auto, backtracking-only and linear-when-eligible host policies. AC3: no upstream fix was needed, since regress is now only a dev-dependency test reference; test262 RegExp suites stay 2502/2502 in all three modes.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Fixed in our own regex engine (TASK-100.31) rather than upstream: /\uD83D\uDE00/ matches the two code units and /\uD83D\u0062?/ finds the lone lead outside u mode, in every matcher mode, and the routing exception is gone (the linear matcher compiles from the same tree). Regression tests are in agnijs-js-regex tests/regress_bugs.rs and agnijs-js-vm tests/regexp.rs. Differential fuzzing at 200000 cases shows zero mismatches between our matchers.
<!-- SECTION:FINAL_SUMMARY:END -->
