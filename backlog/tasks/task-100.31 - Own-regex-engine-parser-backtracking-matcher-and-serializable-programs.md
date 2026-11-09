---
id: TASK-100.31
title: 'Own regex engine: parser, backtracking matcher and serializable programs'
status: To Do
assignee: []
created_date: '2026-11-09T05:17:00Z'
labels:
  - needs-triage
dependencies: []
parent_task_id: TASK-100
ordinal: 270000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Replace regress with a regex engine we own, behind the existing seam (ADR 0013). Scope: own parser and backtracking matcher with backreferences, lookahead and lookbehind, u and v modes, case folding and Annex B legacy syntax; a built-in step budget and empty-loop guards so patterns like /(?:(_|)+){2}K/ cannot hang; compiled regex programs that serialize into bytecode so AOT (TASK-101.5) can embed them; the existing Pike VM linear matcher (TASK-102) keeps working. Oracles: test262 RegExp suites and differential fuzzing against regress. Once it passes, regress can be retired. The four known regress bugs (escaped surrogate pairs outside u mode, \u{...} outside u mode, \b under vi flags, the empty-loop hang) must be fixed or explained. Also folds in the TASK-102 follow-ups: a cost model that understands non-overlapping character sets (regex-long-input is 3x slower than backtracking) and a faster per-character lookup for forced-linear mode. Model: Opus.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 test262 RegExp suites pass 100% in every matcher mode with the new engine as the backtracking matcher
- [ ] #2 Differential fuzzing against regress finds zero mismatches except documented regress bugs; the empty-loop hang is gone
- [ ] #3 Compiled regex programs serialize and load through the bytecode container with a format version bump under ADR 0010
- [ ] #4 regress is removed or becomes an optional test-only reference, and gable-js-bench shows no regression on the regex workloads
<!-- AC:END -->
