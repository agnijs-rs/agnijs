---
id: TASK-103.49
title: RegExp Symbol.replace preallocates captures from a custom exec result length
status: Done
assignee:
  - '@claude'
created_date: '2026-11-13T22:49:00Z'
updated_date: '2026-11-14T01:12:00Z'
labels:
  - bug
  - security
dependencies: []
parent_task_id: TASK-103
priority: medium
ordinal: 314000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the embedder-surface review of 2026-11 (docs/surface-review-2026-11.md), at commit fea680f.

Severity: medium. Script aborts the host process (`memory allocation of N bytes failed`), with or without a memory limit.

Affected code: crates/agnijs-js-vm/src/builtins/regexp.rs around lines 1160 to 1167, in `RegExp.prototype[Symbol.replace]`. It reserves the capture vector from `result.length` of whatever a user-defined `exec` returns. In the same loop, `PropKey::Index(n as u32)` truncates indices above 2^32.

Minimal repro:

    const re = /a/; re.exec = () => ({ length: 2 ** 40, 0: 'a', index: 0 }); 'a'.replace(re, 'b');

Command: `agnijs eval "<the line above>"` (debug build at fea680f).

Expected: the spec reads captures 1 to nCaptures-1 one at a time. The engine should either do that under the interrupt and memory limit, or refuse an absurd capture count with a RangeError. The process must not abort.
Actual: the process aborts on the reservation.

Suggested fix: do not reserve from the attacker's length. Clamp `nCaptures`, for example to 2^32-1, matching the u32 key type, and build the list incrementally while polling the interrupt and the memory limit. Use a key type that cannot truncate for indices of 2^32 and above.

Test that should catch it: a VM test with a custom exec result of length 2^40 and 2^32+1 for `replace`, `match` and `matchAll` (check whether they share the pattern), expecting no abort.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A custom exec result with length 2^40 does not abort the process
- [x] #2 Capture indices of 2^32 and above do not truncate
- [x] #3 A VM test covers replace and any other method that reads exec results the same way
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red test: replace with a custom exec length of 2^32+1, 2^40, 2^53-1; captures read by index; memory limit; interrupt.
2. Size the capture list with check_list_length; poll the interrupt while reading captures.
3. Check split and other exec-result readers.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Red: untrusted_lengths.rs aborted before the fix.

Fix (74a0c56): the replace capture list is sized with Vm::check_list_length (RangeError past 2^22, memory limit checked) and the capture loop polls the interrupt. Since n <= 2^22, capture indices never reach 2^32, so PropKey::Index cannot truncate.

split reads captures up to the result length bounded by its own lengthA < 2^32; its captures loop now polls the interrupt too.

Minor ordering difference: the RangeError comes before result[0] is read, in the pathological case only.

Micro-benchmark: replace and split 1.02x.

Gates: test262 0 regressions in default, --precompile, --snapshot, --only-intl402, the QuickJS-NG baseline and the GC-stress slice; JIT forced and tiered slices (CI filters plus apply, Reflect, TypedArray copyWithin, ArrayBuffer, Atomics, BigInt, RegExp replace/split, Proxy ownKeys) 0 regressions; cargo test -p agnijs-js-vm --features jit green. Bench suite JIT off vs 226e6d1, 8 alternating rounds: geomean 1.005x.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
RegExp.prototype[Symbol.replace] sizes the capture list of a custom exec result through check_list_length, so a length of 2^40 throws a catchable RangeError instead of aborting, and reading captures polls the interrupt; split polls the interrupt in its capture loop too. Tests cover huge lengths, captures read by index, a memory limit and the interrupt for both methods. Commit 74a0c56.
<!-- SECTION:FINAL_SUMMARY:END -->
