---
id: TASK-100.31
title: 'Own regex engine: parser, backtracking matcher and serializable programs'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-09T05:17:00Z'
updated_date: '2026-11-09T21:11:00Z'
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
- [x] #1 test262 RegExp suites pass 100% in every matcher mode with the new engine as the backtracking matcher
- [x] #2 Differential fuzzing against regress finds zero mismatches except documented regress bugs; the empty-loop hang is gone
- [x] #3 Compiled regex programs serialize and load through the bytecode container with a format version bump under ADR 0010
- [x] #4 regress is removed or becomes an optional test-only reference, and gable-js-bench shows no regression on the regex workloads
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Unicode 17 tables generated from the UCD by a Node tool (tools/gen-regex-unicode.mjs) into agnijs-js-regex: properties, scripts, properties of strings, simple case folding and the non-unicode Canonicalize; own CharSet with case closure; syntax.rs checks property names against them (no regress).
2. Own parser: validated pattern -> IR (named groups and \k resolved, modifiers, Annex B, u/v classes with strings). Push.
3. Own backtracking matcher on that IR: explicit backtrack stack, counted loops with registers, ES empty-iteration check, lookaround (atomic), lookbehind backward, backrefs (icase), fast paths (single-char greedy loops, literal prefix scan, first-char skip). Linear matcher compiles from the same IR with CharSet lookups instead of regress predicates. Auto routing: exponential patterns start linear; others backtrack under a step budget and continue on the linear matcher from the current start when it runs out (cost model keeps the non-overlapping-set refinement). Differential fuzzing vs regress (dev-dependency) and linear vs backtracking, bounded. test262 RegExp suites in all three modes. Push.
4. Program serialization: Regex programs encode/decode with verification; the bytecode serializer embeds a compiled program per regex literal; agnijs-bc-7 with an ADR 0010 addendum; VM seeds its regex cache on load. Push.
5. Retire regress: test-only reference (dev-dependency), THIRD-PARTY-NOTICES, ADR 0013 addendum, bench comparison (alternating binaries), full test262 set, gable suites via the path-patched harness, targeted gc-stress subset. Push.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Commits on main: 4d5733e (own parser and Unicode 17 tables), 5231291 (own backtracking matcher; both matchers compile from one parse tree; step-budget routing), 7112c76 (serializable programs, agnijs-bc-7, ADR 0010 addendum), 2fd7fa3 (regress retired to a dev-dependency test reference, ADR 0013 addendum), plus the commit that closes TASK-105 to TASK-108.

AC1: test262 RegExp directories 2502/2502 under auto, backtracking-only and linear-when-eligible (26 skipped for unsupported features, as before). Full test262 42231/42231 from source and with --precompile, intl402 981/981.

AC2: at 200000 cases in release, backtracking vs linear vs Auto compares 14.7M outcomes and backtracking vs regress compares 14.6M, with zero mismatches outside the documented regress divergences (TASK-105..108 plus six found here, pinned in tests/regress_bugs.rs). The /(?:(_|)+){2}K/ hang is gone (empty-iteration check). The program fuzz round-trips 200000 programs and loads 800000 corrupted copies without a panic.

AC3: Regex::program_bytes / from_program_bytes with a verifier; agnijs-bc-7 carries each distinct regex literal program and the VM seeds its regex cache on load; a malformed program is BytecodeCorrupt.

AC4: regress is a dev-dependency only. agnijs-js-bench, default engine, alternating binaries, median of 3x7, against the commit before the new matcher: regex 153.9 -> 105.3 ms, regex-tokenize 46.9 -> 31.7, regex-long-input 7.61 -> 1.63, regex-redos 0.01 -> 0.01. One exception, for review: forced backtracking (engine agnijs-backtracking) on the catastrophic /(a+)+$/ workload is 255.6 ms against regress 116.0 ms. Auto never backtracks that pattern, so only hosts that choose BacktrackingOnly see it, and it is exponential on either engine.

Also verified: Gable suites via the path-patched harness 758 passed, with the two known timing failures (yjs perf, highlighter linearity); harness manifest and lockfile restored. GC stress: the VM regexp, bytecode and engine unit tests under AGNIJS_GC_STRESS=1, and 549 regex test262 tests under AGNIJS_GC_STRESS=16 from source and precompiled.

Finding, not tracked yet: syntax::validate and the tree compiler recurse on group nesting and overflow a 2 MB stack at about 2100 and 1500 nested groups. Recorded in the ADR 0013 addendum; a follow-up task needs user approval.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Replaced regress behind the ADR 0013 seam with our own regex engine in agnijs-js-regex. It has Unicode 17 tables generated from the UCD, a parser that resolves names and folds case into the tree, shared character sets, and a backtracking matcher with an explicit stack (backreferences, lookaround, u/v, Annex B, empty-iteration guard). The Pike VM now compiles from the same tree. Auto routing runs backtracking under a step budget and finishes on the linear matcher when the budget runs out; nested-quantifier patterns go straight to linear. Compiled programs serialize with a verifier and travel in agnijs-bc-7 bytecode (ADR 0010 addendum), which seeds the VM regex cache on load. regress is now a dev-dependency used as the differential-test reference (ADR 0013 addendum, THIRD-PARTY-NOTICES). TASK-105..108 and six further regress divergences are fixed and pinned by tests. test262 is unchanged at 100% (RegExp suites in all three modes, full suite from source and precompiled, intl402), the Gable suites pass, and the default-engine regex benchmarks are 31-78% faster. For review: forced backtracking on the catastrophic /(a+)+$/ is 2.2x slower than regress was (Auto unaffected). Possible follow-up: a nesting-depth limit for the pattern validator and compiler, which overflow the stack at about 1500-2100 nested groups.
<!-- SECTION:FINAL_SUMMARY:END -->
