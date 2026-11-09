---
id: TASK-103.11
title: Isolates, shareable heaps and structured clone
status: To Do
assignee: []
created_date: '2026-11-09T02:15:00Z'
labels:
  - needs-triage
dependencies: []
parent_task_id: TASK-103
ordinal: 246000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Research and design for sending heaps between threads, multiple isolates running in parallel, structured clone and transferables, and Workers. The spec lists multi-threaded sharing as a v1 non-goal, and this touches the garbage collector, the object model and the host interface deeply, so a mistake is costly. Start with a design note and a prototype of a Send isolate; no commitment to Workers. Model: Fable, because it is a cross-cutting GC and object-model redesign where an error is expensive.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 An ADR evaluates isolates, Send heaps and structured clone against the current GC and handle model, with a prototype and a go, no-go or defer recommendation
- [ ] #2 If go: follow-up tasks are written, each with tests and a data-race check (loom or miri where it applies)
<!-- AC:END -->
