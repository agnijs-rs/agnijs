---
id: TASK-103.44
title: BigInt.asIntN and asUintN with a huge bit count abort the process
status: To Do
assignee: []
created_date: '2026-11-13T22:48:00Z'
labels:
  - bug
  - security
dependencies: []
parent_task_id: TASK-103
priority: medium
ordinal: 309000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the embedder-surface review of 2026-11 (docs/surface-review-2026-11.md), at commit fea680f.

Severity: medium. Any script can abort the host process; this is an allocation-failure abort, not a panic, so `catch_unwind` and the C API's panic guard cannot catch it. It also bypasses the memory limit. It is distinct from TASK-103.25 (large native allocations): this allocation site is not counted or bounded.

Affected code: crates/agnijs-js-vm/src/bigint.rs, the asIntN / asUintN helpers (about lines 107 to 133; the shifts at lines 113 and 127 to 128). `bits` comes from `to_index` in builtins/bigint.rs (about lines 57 to 69), which allows up to 2^53-1.

Minimal repro (p6_asintn.js):

    try { console.log(BigInt.asIntN(2 ** 53 - 1, -5n)); } catch (e) { console.log("caught", e.name); }

Command: `agnijs run p6_asintn.js`.

Expected: `-5`. asIntN of a value that already fits returns it unchanged. asUintN(bits, x) for x >= 0 returns x. For negative x with a huge `bits`, a catchable RangeError is acceptable, since the result is a 2^53-bit number.
Actual: exit 127, `memory allocation of 1125899906842632 bytes failed`. `BigInt.asUintN(2**53-1, -5n)` and even `BigInt.asIntN(2**53-1, 0n)` do the same, and `BigInt.asUintN(2**40, 5n)` tries to allocate 137438953488 bytes. Below the abort threshold, `BigInt.asUintN(2**33, -1n)` succeeds under `--max-memory 16M` with a 1 GiB peak, so the limit does not bound the result either.

Root cause: the modulus `BigInt::from(1) << bits` is built for any `bits`. The shift operators already cap their exponent at 2^30 bits, but these two functions do not.

Suggested fix:
- If `bits >= x.bits() + 1` (the value already fits), return x for asIntN, and for asUintN with x >= 0.
- For asUintN with negative x, refuse `bits` above the same cap as `<<` with a RangeError.
- Below the cap, mask with bit operations instead of materialising the modulus.
- Charge any large result to the memory limit.

Test that should catch it: a VM test for `asIntN` and `asUintN` with `bits` of 2^53-1, 2^40 and 2^31, with positive and negative values, that expects either the spec value or a RangeError, within a time bound.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 asIntN and asUintN with bits up to 2^53-1 return the spec value or a catchable RangeError without a large allocation
- [ ] #2 Large results are charged to the memory limit
- [ ] #3 A VM test covers bits of 2^31, 2^40 and 2^53-1 with positive, zero and negative values
<!-- AC:END -->
