---
id: TASK-100.36
title: >-
  Tiered JIT is 6 to 13% slower than at 1d172aa on prop-write-alloc, regex and
  method-calls
status: Done
assignee:
  - '@claude'
created_date: '2026-11-13T04:45:00Z'
updated_date: '2026-11-14T15:54:00Z'
labels:
  - performance
  - jit
dependencies: []
references:
  - docs/js-engine-performance-findings.md
parent_task_id: TASK-100
priority: low
type: bug
ordinal: 298000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found in the 2026-11-12 re-benchmark (docs/js-engine-performance-findings.md). With the JIT build and AGNIJS_JIT=on, five alternating rounds against 1d172aa give: prop-write-alloc 1.11x its time at 1d172aa (range 1.11 to 1.18), regex 1.08x (1.07 to 1.11), regex-long-input 1.13x (1.09 to 1.16), method-calls 1.06x (1.05 to 1.10). The same workloads with the JIT off are within noise (0.93 to 1.00), so this is specific to the JIT path (calls in machine code, e9bf4e8, is the likely suspect). The JIT also raised peak RSS in tiered mode: rxjs-higher-order 13 to 39 MiB, template-mount 41 to 68 MiB, array-methods 9 to 18 MiB, regex 10 to 23 MiB; with the JIT off RSS is unchanged.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Cause of each slowdown found by bisecting 1d172aa..main with AGNIJS_JIT=on
- [x] #2 prop-write-alloc, regex and method-calls are back within 3% of their 1d172aa tiered times, or the trade-off is documented
- [x] #3 Tiered peak RSS growth on rxjs-higher-order and template-mount explained (code size, compile queue or both)
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Build bench (--features jit) at 1d172aa, e9bf4e8 and main from one checkout path; measure interleaved rounds with AGNIJS_JIT=on on the four workloads.
2. Bisect 1d172aa..main per workload (JIT commits first, then the VM commits between).
3. Profile or count (AGNIJS_JIT_STATS) the culprit; fix what is fixable without touching the 103.34 to 103.36 fixes.
4. Explain tiered peak RSS (code size against compile queue) with measurements.
5. Document trade-offs with numbers in docs/js-engine-performance-findings.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Measured 2026-11-14 (bench built with --features jit at each commit from one checkout path; 9 interleaved rounds; median of each run's median sample).

1. prop-write-alloc, regex-long-input, method-calls: not the JIT. With AGNIJS_JIT unset the same binaries show the same ratios against 1d172aa as with AGNIJS_JIT=on (at 107fa7c, the commit the report measured: off 1.066 / 1.124 / 1.075, on 1.076 / 1.120 / 1.089). Nothing is compiled on prop-write-alloc and regex-long-input in either build (stats: compiled 0, unprofitable 1). The ratio moves from build to build with no JIT change: regex-long-input off-mode across 11 commits is 1.00, 1.02, 1.06, 0.99, 0.94, 1.12, 1.12, 1.12, 0.95, 1.10, 1.10; fa02ecc, the commit before e9bf4e8, is 1.03 / 1.02 / 1.07 and e9bf4e8 itself 0.99 / 1.06 / 1.02. It is code layout of the Rust build (the feature-off builds of 1d172aa, e9bf4e8 and main give 1.76, 1.62, 1.56 ms on regex-long-input).

2. regex: real, and from 8459ae4 (TASK-103.28), not e9bf4e8. Natives poll the interrupt counter once per step over their input since then; a compiled run reads its work off that counter, so the polls of exec/replace counted as back-edges (back_edges 20,201 at 4cb792f, 1,401,892 from 8459ae4 on, same 1.1 M call outs) and the loop was never given up. on/off in one binary: 1.044 at main.

3. Every loop over objects was compiled twice: a temporary the bytecode reuses (pts for the element load, s + p.x for the add) held a number at the back-edge, was guessed a number, and failed at the first store of the object (1 deopt, second compile).

4. Peak RSS: compiler working memory. Working set around each Compiled::compile_calling (throwaway probe): regex 9.7 -> 20.1 MiB peak inside the first compile (68.9 KB of code), 12.9 MiB current after; method-calls 6.8 -> 12.6; loop-arith 6.6 -> 9.7 (4.9 KB of code: about 2.5 MiB is the code generator's first use). There is no compile queue; code is 0.06 to 0.3 MiB.

Correction to the note above: the working-set figures are 9.3 to 19.6 MiB peak and 12.6 MiB after for regex (the earlier figures divided KiB by 1000). Gable benches, harness built against this tree, one run: template-mount 44.4 MiB off, 71.6 on (one 157 KB block compiled twice, 144 ms, then given up); rxjs-higher-order 29.0 off, 41.7 on (20 blocks, 286 KB, 19 given up).

After the two commits (9 rounds, AGNIJS_JIT=on, over 1d172aa): prop-write-alloc 1.018 (JIT off in the same binaries 1.010), regex 0.975 (main before 1.009), method-calls 1.031 (off 1.018), regex-long-input 1.130 (off 1.133: layout, nothing compiled). AC 2 is met by regex and prop-write-alloc and documented for method-calls, whose 3.1% is there with the JIT off too.

Gates: test262 default, --precompile, --snapshot, --only-intl402 and QuickJS-NG 0 regressions; forced and tiered slices 0 regressions; GC-stress slice 0; VM suite forced green; bytecode_fuzz and jit_workloads green; Miri 9 tests; jit-fuzz default run 541 programs, 0 divergences; clippy reports nothing in the touched files.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Bisected 1d172aa..main. Three of the four slowdowns (prop-write-alloc, regex-long-input, method-calls) are the layout of the Rust build, not the JIT: they are the same with the JIT off in the same binaries, and come and go across commits that do not touch the code involved. regex was the JIT, from 8459ae4 (TASK-103.28), not e9bf4e8: natives' interrupt polls counted as the compiled loop's back-edges, so the loop was never given back to the interpreter. Fixed (Vm::jit_uncounted): tiered regex 1.009 to 0.975 of 1d172aa. Also fixed: a temporary the bytecode reuses for an object and a number was guessed a number, so nearly every loop over objects was compiled twice (compile time roughly halved on five workloads). Tiered peak RSS is the code generator's working memory during a compilation, about 150 bytes per byte of code, measured around each compilation; it stays, and docs/js-engine-performance-findings.md (Tiered JIT follow-up, 2026-11-14) has the numbers. Cost accepted: forced mode on code that is mostly call outs is 2 to 5% slower (prop-write-alloc, string-concat); geomeans on 0.995, force 1.000, off 1.001 over main before. Commit ad7a1b8.
<!-- SECTION:FINAL_SUMMARY:END -->
