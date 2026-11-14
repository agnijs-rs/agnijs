---
id: TASK-103.44
title: BigInt.asIntN and asUintN with a huge bit count abort the process
status: Done
assignee:
  - '@claude'
created_date: '2026-11-13T22:48:00Z'
updated_date: '2026-11-14T01:12:00Z'
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
- [x] #1 asIntN and asUintN with bits up to 2^53-1 return the spec value or a catchable RangeError without a large allocation
- [x] #2 Large results are charged to the memory limit
- [x] #3 A VM test covers bits of 2^31, 2^40 and 2^53-1 with positive, zero and negative values
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red test: bits 2^31, 2^40, 2^53-1 with positive, zero, negative values; a lowered memory limit.
2. Return x unchanged when it already fits; else mask only the low bits.
3. asUintN of a negative value: RangeError past 2^30 bits (the << maximum), else charge the memory limit before building.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Red: untrusted_lengths.rs aborted with 0xc0000409 before the fix.

Fix (74a0c56): when the input already fits in bits, it is returned as is; otherwise only the low bits are masked. asUintN of a negative value really has bits bits: past 2^30 (bigint::MAX_BITS, the same maximum as <<) it throws RangeError, and below that the result size is charged with check_memory before it is built.

Limit: the memory check is up front only. The collector still counts each live BigInt at a fixed size; that is TASK-103.50.

Micro-benchmark: asIntN/asUintN 0.81x, wide widths 0.53x.

Gates: test262 0 regressions in default, --precompile, --snapshot, --only-intl402, the QuickJS-NG baseline and the GC-stress slice; JIT forced and tiered slices (CI filters plus apply, Reflect, TypedArray copyWithin, ArrayBuffer, Atomics, BigInt, RegExp replace/split, Proxy ownKeys) 0 regressions; cargo test -p agnijs-js-vm --features jit green. Bench suite JIT off vs 226e6d1, 8 alternating rounds: geomean 1.005x.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
BigInt.asIntN and asUintN no longer build a 2^bits mask. A value that fits is returned unchanged and the rest mask only the low bits, so any width up to 2^53-1 gives the spec value. asUintN of a negative value throws RangeError past 2^30 bits and is charged to the memory limit below that. Tests cover widths 2^31, 2^40 and 2^53-1 with positive, zero and negative values, wrap cases and a 1 MiB limit. Commit 74a0c56. Persistent BigInt accounting stays with TASK-103.50.
<!-- SECTION:FINAL_SUMMARY:END -->
