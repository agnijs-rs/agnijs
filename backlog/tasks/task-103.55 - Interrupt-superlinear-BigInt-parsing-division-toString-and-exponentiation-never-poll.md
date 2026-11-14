---
id: TASK-103.55
title: >-
  Interrupt: superlinear BigInt parsing, division, toString and exponentiation
  never poll
status: Done
assignee:
  - '@claude'
created_date: '2026-11-13T22:49:00Z'
updated_date: '2026-11-14T10:21:00Z'
labels:
  - bug
  - security
dependencies: []
parent_task_id: TASK-103
priority: medium
ordinal: 320000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the embedder-surface review of 2026-11 (docs/surface-review-2026-11.md), at commit fea680f.

Severity: medium (a denial of service that the wall-clock interrupt cannot stop). docs/security-model.md, "Bounded native work between polls", allows native work without polling only when it is at most linear in one string or buffer. BigInt parsing, division, multiplication, exponentiation and `toString` are superlinear, and none of them poll.

Affected code: crates/agnijs-js-vm/src/bigint.rs and builtins/bigint.rs (parsing, `/`, `%`, `*`, `**`, `toString`). `**` accepts any exponent that fits in a u32.

Minimal repros (debug build at fea680f, one call each):

    BigInt('9'.repeat(1e6))                                  more than 120 s (timed out)
    ((1n << 8000000n) - 1n) / ((1n << 4000000n) - 3n)        93 s
    (1n << 2000000n).toString()                              6.9 s
    (1n << 1000000n) ** 100000n, 3n ** 100000000n            more than 30 s (timed out)

Command: `agnijs eval "<one line>"`.

Expected: an interrupt request (`Host::interrupt` or `agnijs_engine_request_interrupt`) stops the call within a bounded time. Or operand sizes are capped so the call is bounded. V8 caps BigInts at 2^30 bits and its algorithms are subquadratic. A release build is faster by a constant factor only, so a larger input reproduces the same time.
Actual: one call runs for minutes with no poll.

Suggested fix: cap the result size of `*`, `**` and `<<` with a RangeError (as `<<` already does at 2^30 bits; check `**` up front from the bit length of base × exponent). Poll the interrupt inside the long loops (schoolbook division and multiplication, radix conversion). Or switch to subquadratic algorithms for parsing and `toString`. Then add BigInt to the documented list of native work that polls.

Test that should catch it: interrupt_native.rs tests that start each repro with an interrupt requested after 100 ms and assert the call ends with Interrupted within a second or two.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 An interrupt requested during each repro in the description ends the call within a bounded time, or the operand size is refused up front
- [x] #2 docs/security-model.md lists BigInt work in the interrupt section accurately
- [x] #3 interrupt_native.rs covers the repros
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red: interrupt_native.rs repros (parse, /, %, *, **, toString, String, Intl) not interrupted within 2 s. 2. Polled block algorithms in bigint.rs reporting digit work to Vm::poll_work. 3. Refuse * and ** results past 2^30 bits up front and check the memory limit. 4. Route every BigInt-to-string and string-to-BigInt site through them. 5. Exactness tests against shift/add identities. 6. Document in security-model.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Red first: (1n << 2000000n).toString() finished in 6.5 s with no interrupt; BigInt parsing ran past 120 s. num-bigint operations cannot be polled from inside, so large operands go through chunked algorithms in bigint.rs reporting digit work to Vm::poll_work. Not polled, documented: BigInt literals in source (bounded by source length) and the decimal conversion when eval returns a BigInt to the host. Commit c4abee5. Gates: agnijs-js-vm and agnijs-js-regex suites green; clippy: no warnings on changed lines; test262 0 regressions in default, --precompile, --snapshot, --only-intl402, vs QuickJS-NG, the GC-stress slice and the JIT force and tiered slices.

Benchmarks (JIT off, 4 interleaved rounds against edbdb50, 26 workloads): geomean +0.23%. The polled algorithms inlined into the dispatch loop with op_arith cost 3 to 6% on prop-polymorphic, method-calls and regex-tokenize; b381df5 moves the BigInt arm out of line and they are back at the starting times.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
BigInt parsing, *, /, %, ** and conversion to a string run in steps of about 2^22 digit operations for large operands, each reported to Vm::poll_work, which asks the host about once per 2^20; small operands keep num-bigint's path. * and ** refuse results past 2^30 bits up front and check the memory limit. Power-of-two radix parsing is now linear. interrupt_native.rs interrupts every repro within 2 s, checks oversized results throw RangeError at once, and checks exact results against shift/add identities. Commit c4abee5.
<!-- SECTION:FINAL_SUMMARY:END -->
