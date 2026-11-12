---
id: TASK-103.32
title: >-
  Map and Set: reclaim deleted-entry slots so set/delete churn does not grow
  memory
status: To Do
assignee: []
created_date: '2026-11-12T16:28:00Z'
labels:
  - bug
dependencies: []
parent_task_id: TASK-103
priority: medium
ordinal: 296000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found while doing TASK-103.25. A deleted Map or Set entry leaves an empty slot that is kept until the collection is freed, so a loop of m.set(k,v); m.delete(k) grows memory without bound (about 32 bytes per round, not yet measured) and, once collection slots are counted in the memory limit, would hit the limit. Reclaiming slots needs a change to how Map/Set iterators track their position (e.g. compaction with iterator index fix-up). Must keep iteration order and live-iterator semantics per spec.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Red test: set/delete churn on a Map and a Set keeps memory_usage bounded
- [ ] #2 Live iterators stay correct across deletes and compaction (test262 collections tests and new tests)
- [ ] #3 Benchmark geomean within 1%, test262 0 regressions in all modes
<!-- AC:END -->
