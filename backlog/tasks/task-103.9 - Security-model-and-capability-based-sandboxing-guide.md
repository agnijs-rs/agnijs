---
id: TASK-103.9
title: Security model and capability-based sandboxing guide
status: To Do
assignee: []
created_date: '2026-11-09T02:14:00Z'
labels:
  - ready-for-agent
dependencies: []
parent_task_id: TASK-103
ordinal: 244000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Write and publish the security model: the engine has no ambient authority (no DOM, no globals beyond ECMAScript, host capabilities granted one by one), what the limits protect against (memory, instructions, wall time, stack, regex), what the engine does not protect against (side channels, host bugs), and how bytecode must be authenticated by the host (the checksum is not a signature, see the ADR 0010 addendum). Review the engine against the model and file issues. Model: Sonnet for the document and review, Opus for any fix.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A security model document in docs/ states the threat model, the guarantees, the non-guarantees and the embedder checklist
- [ ] #2 The engine is reviewed against it; every gap found is filed as a task, and the document is linked from the embedding guide
<!-- AC:END -->
