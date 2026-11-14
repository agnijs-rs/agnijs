---
id: TASK-103.58
title: 'JIT differential fuzz target: interpreter vs AGNIJS_JIT=on vs force'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-14T05:28:00Z'
updated_date: '2026-11-14T07:39:00Z'
labels:
  - jit
  - testing
dependencies: []
parent_task_id: TASK-103
priority: medium
ordinal: 322000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The JIT soundness review (docs/jit-review-2026-11.md, TASK-103.34 to TASK-103.36) found its wrong-result defect with Node-script differential generators that were kept outside the repository, so nothing in the repository re-checks the JIT against the interpreter on random programs. On the starting binary those generators diverged on 3 of 210 programs; on the fixed one, 0 of 800. Without a committed, seeded target a regression in liveness, speculation or the call thunk would only be found by luck. Part of the fuzzing effort of TASK-103.18, scoped to the JIT.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A Node script runs seeded random programs from the review generators under the interpreter, AGNIJS_JIT=on and AGNIJS_JIT=force and compares stdout, exit status and the first stderr line; a seed reproduces its program exactly
- [x] #2 The default run is bounded to about two minutes; options lengthen it, select generators, add a warm-up variant, GC stress and a Node oracle
- [x] #3 A divergent program is saved with a report and a minimized copy, and the run exits non-zero
- [x] #4 Hand-written regression programs, including the three pre-fix divergences, run in the same comparison
- [x] #5 The script was shown to report divergences on a pre-fix binary and none on the fixed one
- [x] #6 Documented, and wired into CI only if a short run is cheap and not flaky
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. tools/jit-fuzz/: generators from the review (handlers = difffuzz, mixed = gen, numeric = gen2) as modules with explicit options, a driver jit-fuzz.mjs (seeded, time or count bound, jobs, warm, stress, node oracle, ddmin line minimizer), regressions/ with the three pre-fix programs and the review hand probes.
2. Check against the pre-fix binary kept from the review (fea680f build) and a fresh build of main.
3. Doc docs/jit-fuzz.md, link from the review doc and the deopt design doc; CI step in the jit job on ubuntu if cheap.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Pre-fix binary: the review's clean build of fea680f (kept in a session scratchpad), so no rebuild of 32efdce~4 was needed. Default run on it: 7 divergences (six TASK-103.34 regressions + generated handlers-17, minimized 85 -> 17 lines). Fixed (32efdce): default --node 512 programs, --stress 164 programs, 0 divergences. Seed rotation changed so every seed runs all generators (with s mod 3, handlers-17 was never reached). gen-mixed emitted SyntaxErrors in ~25% of programs (use strict + default param, -1 ** x, --1, typeof this ** x); fixed without changing RNG draws, verified 790/999 seeds byte-identical before the literal fix and 0 syntax errors in 3000 seeds after.

CI run 43568554508 (edbdb50): all jobs green; the fuzz step took 43 s on ubuntu-latest including the CLI build (fuzz itself 23 s: 10 regressions + seeds 1..40, 120 programs, 0 divergences).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added tools/jit-fuzz: a seeded, bounded differential fuzz target that runs random programs (the JIT review's three generators) and ten regression programs under the interpreter, AGNIJS_JIT=on and AGNIJS_JIT=force, with optional warm-up, GC stress and Node oracle; divergences are saved with a report, a repro command and a ddmin-minimized copy. Recovered the three pre-fix divergences (seeds 17, 1006, 1019) and minimized them to 17-19 lines. On the review's pre-fix build (fea680f) the default run reports 7 divergences (six TASK-103.34 regressions + generated handlers-17); on main, 512 programs with --node and 164 under GC stress agree. Fixed two generator flaws found on the way: seed rotation that never reached handlers-17, and ~25% SyntaxError programs in the mixed generator. Documented in docs/jit-fuzz.md; CI jit job (Ubuntu) runs --count 40 in 43 s. Risk: TASK-103.34 shows in ~1 of 70 handlers programs, so the CI run is a smoke test; long runs are documented.
<!-- SECTION:FINAL_SUMMARY:END -->
