---
id: TASK-105
title: 'Regress: escaped surrogate pairs outside u mode never match'
status: To Do
assignee: []
created_date: '2026-11-09T05:07:00Z'
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
- [ ] #1 /\uD83D\uDE00/ matches the string "\uD83D\uDE00" outside u mode, and /\uD83D\u0062?/ matches the lone lead in "a\uD83D\uDE00", on both matchers and under every MatcherMode
- [ ] #2 A regression test covers both patterns (in agnijs-js-regex tests and the VM regexp tests), and the linear-eligibility routing exception for an escaped lead surrogate followed by another \u escape is removed, with the differential fuzzer still showing zero mismatches
- [ ] #3 If the fix is upstream, the regress dependency is bumped (or patched with a recorded reason) and test262 RegExp suites and baselines are unchanged
<!-- AC:END -->
