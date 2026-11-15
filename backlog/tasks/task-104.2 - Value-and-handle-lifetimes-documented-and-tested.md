---
id: TASK-104.2
title: 'Value and handle lifetimes: documented and tested'
status: To Do
assignee:
  - '@claude'
created_date: '2026-11-15T22:22:00Z'
labels: []
dependencies: []
parent_task_id: TASK-104
type: feature
ordinal: 325000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Embedders hold Handles in their own structures for a long time (ADR 0006). What keeps a heap value alive, when slots are released, what collect_garbage does, and what happens to handles that outlive their engine is only partly written down and only the release path is tested.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 The rustdoc of Handle, Value, reclaim_handles and collect_garbage states when a handle keeps a value alive, what reclaim_handles and collect_garbage guarantee (including when called from inside a host function), what happens to handles after the engine is dropped, and what a RealmId is valid for
- [ ] #2 Tests show: a held handle keeps its object alive across collections; dropping it lets the object be collected; handles survive and drop safely after their engine is gone; a handle given to another engine or an unknown realm id is an error, not a panic
<!-- AC:END -->
