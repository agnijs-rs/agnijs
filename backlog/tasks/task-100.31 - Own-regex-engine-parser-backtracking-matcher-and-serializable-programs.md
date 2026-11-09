---
id: TASK-100.31
title: 'Own regex engine: parser, backtracking matcher and serializable programs'
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-09T05:17:00Z'
updated_date: '2026-11-09T19:43:00Z'
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

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Unicode 17 tables generated from the UCD by a Node tool (tools/gen-regex-unicode.mjs) into agnijs-js-regex: properties, scripts, properties of strings, simple case folding and the non-unicode Canonicalize; own CharSet with case closure; syntax.rs checks property names against them (no regress).
2. Own parser: validated pattern -> IR (named groups and \k resolved, modifiers, Annex B, u/v classes with strings). Push.
3. Own backtracking matcher on that IR: explicit backtrack stack, counted loops with registers, ES empty-iteration check, lookaround (atomic), lookbehind backward, backrefs (icase), fast paths (single-char greedy loops, literal prefix scan, first-char skip). Linear matcher compiles from the same IR with CharSet lookups instead of regress predicates. Auto routing: exponential patterns start linear; others backtrack under a step budget and continue on the linear matcher from the current start when it runs out (cost model keeps the non-overlapping-set refinement). Differential fuzzing vs regress (dev-dependency) and linear vs backtracking, bounded. test262 RegExp suites in all three modes. Push.
4. Program serialization: Regex programs encode/decode with verification; the bytecode serializer embeds a compiled program per regex literal; agnijs-bc-7 with an ADR 0010 addendum; VM seeds its regex cache on load. Push.
5. Retire regress: test-only reference (dev-dependency), THIRD-PARTY-NOTICES, ADR 0013 addendum, bench comparison (alternating binaries), full test262 set, gable suites via the path-patched harness, targeted gc-stress subset. Push.
<!-- SECTION:PLAN:END -->
