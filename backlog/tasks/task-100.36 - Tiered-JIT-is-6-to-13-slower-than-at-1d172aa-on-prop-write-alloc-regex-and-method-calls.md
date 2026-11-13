---
id: TASK-100.36
title: >-
  Tiered JIT is 6 to 13% slower than at 1d172aa on prop-write-alloc, regex and
  method-calls
status: To Do
assignee: []
created_date: '2026-11-13T04:45:00Z'
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
- [ ] #1 Cause of each slowdown found by bisecting 1d172aa..main with AGNIJS_JIT=on
- [ ] #2 prop-write-alloc, regex and method-calls are back within 3% of their 1d172aa tiered times, or the trade-off is documented
- [ ] #3 Tiered peak RSS growth on rxjs-higher-order and template-mount explained (code size, compile queue or both)
<!-- AC:END -->
