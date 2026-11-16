---
id: TASK-110
title: Interpreter and runtime performance after the tier close-out
status: To Do
assignee: []
created_date: '2026-11-16T14:28:00Z'
labels:
  - performance
dependencies: []
ordinal: 340000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
ADR 0028 (TASK-101.4 no-go) ends the JIT tiers at the baseline and sends performance work to the interpreter and runtime, where every platform and tier gains. Measurements are in docs/js-engine-performance-findings.md, section "Performance-tier close-out, 2026-11-16". Subtasks are in the ADR order of expected return on Gable workloads.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Each subtask lands with before/after numbers on the micro-benchmarks and the Gable benches, in JIT-off and JIT-on modes
- [ ] #2 The findings document is updated with the cumulative result and the remaining gap to QuickJS-NG
<!-- AC:END -->
