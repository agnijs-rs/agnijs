---
id: TASK-103.59
title: 'Number to string: break exact shortest-digit ties to the even digit (match V8)'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-14T17:31:00Z'
updated_date: '2026-11-14T18:24:00Z'
labels:
  - bug
  - conformance
dependencies: []
parent_task_id: TASK-103
priority: low
ordinal: 323000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
String(2 ** -25) gives 2.9802322387695313e-8; V8 and the spec's recommendation give 2.9802322387695312e-8 (exact tie between ...312 and ...313; the even digit is preferred). Both round-trip and test262 accepts either, so this is a byte-for-byte difference with Node only (docs/jit-review-2026-11.md observations; known difference in docs/jit-fuzz.md). Fix the tie-break in the shortest-digits step of Number::toString (find it with grep for the number-to-string routine in agnijs-js-vm / runtime), then drop the known-difference note from docs/jit-fuzz.md and the review doc.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Failing test first: exact-tie values (2 ** -25 and other powers of two, plus a few negative and large exponents) print the even digit and match Node
- [x] #2 No change to any non-tie output: test262 0 regressions in default, --precompile, --snapshot, --only-intl402 and vs QuickJS-NG; a sweep of random doubles compared against Node shows no new differences
- [x] #3 Number-printing benchmark within 1% of the starting commit
- [x] #4 Known-difference notes removed from docs/jit-fuzz.md and docs/jit-review-2026-11.md; jit-fuzz --node run shows no String(2 ** -25) difference
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Failing tests in agnijs-js-parser number.rs (exact ties incl. powers of two, negative and large exponents; Node's outputs) and the determinism corpus.
2. shortest_digits (shared by Number::toString and toExponential()): after Rust's {:e} (rounds ties up), if the last digit is odd and x is an exact decimal of at most 18 digits (m * 5^-q, q in -25..-1), check for an exact tie and take the even neighbour when it round-trips (analytic unless the significand is a power of two, then parsed back).
3. Sweep ~945k doubles against Node before/after (two seeds); number-printing micro-benchmark and bench suite geomean, 4 interleaved rounds, JIT off.
4. Gates: clippy, test262 (default, --precompile, --snapshot, --only-intl402, quickjs), GC-stress slice, jit-fuzz --node.
5. Drop the known-difference notes from docs/jit-fuzz.md and docs/jit-review-2026-11.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Cause: Rust's shortest {:e} rounds a tie up. Fix in agnijs-js-parser number.rs shortest_digits (also used by toExponential() through the VM's shortest_digits). Only odd last digits of exact decimals x = m*2^q (m odd, -25 <= q < 0, m*5^-q < 10^18) are checked: a tie is impossible for q >= 0 (the candidates would be 5*10^q away, more than the half-ulp). The even neighbour is taken when x is exactly halfway and it round-trips: decided analytically (symmetric interval) unless the significand is a power of two, then parsed back. 2 ** -24 keeps ...063, as Node does: its even neighbour does not round-trip.

AC 1: parser tests exact_ties_take_the_even_digit (Node's values, exponents -8 to +15, 2^-25, 5*2^-23, 9*2^-23, 2^-24, 2^-21) and the oracle test with an independent exact ({:.800e}) tie rule over 100k exact decimals plus the existing 600k values; determinism corpus numbers.js has a new line. Red before the fix, green after.
AC 2: sweep against Node (String and toExponential, JIT off) of 945,158 values (seed 1: edges, every power of two, 40k exact decimals, 900k random bit patterns, integers and short decimals): 381 differences before, all exact ties, 0 after; second seed 945,202 values, 353 before, 0 after. test262 0 regressions: default 43192/43192, --precompile and --snapshot the same, --only-intl402 981/981, quickjs 43073 passed / 84 failed as its baseline, GC-stress slice (AGNIJS_GC_STRESS=16) 5959/5959. Clippy -D warnings on agnijs-js-parser and agnijs-js-vm: nothing in the touched files (other files in agnijs-js-parser have older clippy errors).
AC 3: number-printing micro-benchmark (String() over 80k doubles of four kinds, best of 5, JIT off, 4 interleaved rounds): 1.0045 and 1.0015 of the starting commit (a first version that called out of line for every candidate exponent was 1.021). Bench suite geomean, 4 interleaved rounds, JIT off: 0.987.
AC 4: notes removed from docs/jit-fuzz.md and docs/jit-review-2026-11.md. jit-fuzz --node: a file printing String(2 ** -25) agrees with Node (0 divergences), and the default run, 10 regressions plus 447 programs: 0 divergences.

Also seen: stack_budget's four refuses_deep_input tests fail under cargo test --release on the starting commit too (regex nesting cap reached before the stack check); not touched.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Number::toString and toExponential() now take the even digit on an exact tie between two shortest candidates, as V8 does: String(2 ** -25) is 2.9802322387695312e-8. Rust's {:e} rounds ties up. The fix checks odd digits of the only doubles that can tie (exact decimals of at most 18 digits) and takes the even neighbour when it round-trips. Sweeps of about 945k doubles against Node on two seeds went from 381 and 353 differences, all ties, to 0. test262 shows 0 regressions in all five modes and the GC-stress slice; number printing is within 0.5% and the suite geomean is 0.987. Known-difference notes removed from the JIT docs. Commit e55115d.
<!-- SECTION:FINAL_SUMMARY:END -->
