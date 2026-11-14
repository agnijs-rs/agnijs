---
id: TASK-103.50
title: 'Memory limit: the collector counts each live BigInt as 48 bytes'
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
ordinal: 315000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the embedder-surface review of 2026-11 (docs/surface-review-2026-11.md), at commit fea680f.

Severity: medium (a memory-limit bypass). docs/security-model.md lists BigInts among the counted values.

Affected code: crates/agnijs-js-vm/src/gc.rs around line 866. The sweep counts every live BigInt as `live_bytes += 48`, while heap.rs around line 723 charges a BigInt's digits when it is allocated. Each collection resets the counter to the swept total, so the digits of every BigInt that survives a collection stop counting.

Minimal repro:

    const b = 1n << (2n ** 24n); const k = []; for (let i = 1; i <= 1000; i++) k.push(b + BigInt(i));

Command: `agnijs run --max-memory 16M <file>` (debug build at fea680f).

Expected: exit 3 (out of memory) early. 1,000 live 2 MiB BigInts is 2 GiB.
Actual: exit 0 with a peak working set of 2,019 MiB, all of it still live.

Suggested fix: count `48 + digits_bytes` (about `bits / 8`) for each live BigInt in the sweep, the same size the allocation charges. Check the sweep's per-kind sizes for other values that carry a variable payload against their allocation charge (strings, BigInts, Map keys).

Test that should catch it: a memory_limit.rs test that keeps N large BigInts alive across forced collections and expects OutOfMemory at the limit.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Live BigInts are counted at their digit size after a collection, as at allocation
- [x] #2 Keeping large BigInts alive across collections under a small limit ends in OutOfMemory
- [x] #3 memory_limit.rs covers it
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red test in memory_limit.rs: 64 live 1 MiB BigInts under 16 MiB, and memory_usage after a collection.
2. One size for a BigInt (BIGINT_BYTES + digit bytes) used by alloc_bigint, the sweep and approx_live_bytes.
3. Audit the other per-kind sweep sizes against their allocation charge.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Red: live_bigints_count_their_digits_across_collections returned Ok(64) and memory_usage grew 96 bytes for a 1 MiB BigInt. Fixed by BigIntData::heap_bytes (BIGINT_BYTES 48 + digit bytes), used by alloc_bigint, the sweep, the native-frame retained estimate and approx_live_bytes. Audit: objects (OBJECT_BYTES + extra_bytes), strings (STRING_BYTES + byte_len), envs (ENV_BYTES + 16/slot) and symbols agree between alloc and sweep; Map key payloads are in MapData::heap_bytes. Note: returning a huge BigInt to the host is slow (decimal conversion, TASK-103.55).

Red first: 64 live 1 MiB BigInts under a 16 MiB limit ran to the end. BigIntData::heap_bytes is now the one size used by the allocation charge, the sweep and memory_usage; the other per-kind sizes already agreed. Commit 841297c. Gates: agnijs-js-vm and agnijs-js-regex suites green; clippy: no warnings on changed lines; test262 0 regressions in default, --precompile, --snapshot, --only-intl402, vs QuickJS-NG, the GC-stress slice and the JIT force and tiered slices.

Benchmarks (JIT off, 4 interleaved rounds against edbdb50, 26 workloads): geomean +0.23%.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
The sweep counted a live BigInt as 48 bytes while allocation charged its digits, so live BigInts fell out of the count at every collection. BigIntData::heap_bytes (48 bytes plus digits) now sizes allocation, sweep and memory_usage alike. memory_limit.rs: 64 live 1 MiB BigInts under 16 MiB end in OutOfMemory, and memory_usage reports and then drops a BigInt's digits. Commit 841297c.
<!-- SECTION:FINAL_SUMMARY:END -->
