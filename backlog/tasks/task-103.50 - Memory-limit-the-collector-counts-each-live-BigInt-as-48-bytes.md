---
id: TASK-103.50
title: 'Memory limit: the collector counts each live BigInt as 48 bytes'
status: To Do
assignee: []
created_date: '2026-11-13T22:49:00Z'
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
- [ ] #1 Live BigInts are counted at their digit size after a collection, as at allocation
- [ ] #2 Keeping large BigInts alive across collections under a small limit ends in OutOfMemory
- [ ] #3 memory_limit.rs covers it
<!-- AC:END -->
