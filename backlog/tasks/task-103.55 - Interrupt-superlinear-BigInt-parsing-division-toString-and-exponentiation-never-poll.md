---
id: TASK-103.55
title: >-
  Interrupt: superlinear BigInt parsing, division, toString and exponentiation
  never poll
status: To Do
assignee: []
created_date: '2026-11-13T22:49:00Z'
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
- [ ] #1 An interrupt requested during each repro in the description ends the call within a bounded time, or the operand size is refused up front
- [ ] #2 docs/security-model.md lists BigInt work in the interrupt section accurately
- [ ] #3 interrupt_native.rs covers the repros
<!-- AC:END -->
