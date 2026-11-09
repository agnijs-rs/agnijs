---
id: TASK-107
title: 'Regress: \b and \B under the vi flags ignore case folding'
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
ordinal: 268000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Under the v and i flags together, \b and \B do not use the case-folded word characters that ECMAScript requires (for example U+017F and U+212A count as word characters when matching with u or v and i). The linear matcher deliberately matches regress here so the two agree; both are wrong. Found by the differential fuzzer of TASK-102 (ADR 0013 addendum, "Divergences found in regress"). The linear matcher is not affected; affected patterns are routed to the backtracking matcher (regress) so both policies agree and behaviour is unchanged. This ticket removes that routing exception by fixing the divergence, either upstream in the regress crate (preferred: report it, send a patch, then bump the dependency) or in the seam or the linear matcher if upstream is unresponsive.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 \b and \B use the case-folded word-character set under (u or v) and i, per the RegExp Matcher semantics (WordCharacters), on both matchers, with a regression test
- [ ] #2 The linear matcher word test is changed together with the backtracking one so the differential fuzzer keeps zero mismatches, and test262 RegExp baselines are unchanged or improved
<!-- AC:END -->
