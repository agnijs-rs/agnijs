---
id: TASK-103.7
title: Post-ES2025 language features (ES2026 and later)
status: To Do
assignee: []
created_date: '2026-11-09T02:14:00Z'
updated_date: '2026-11-09T02:35:00Z'
labels:
  - ready-for-agent
dependencies: []
parent_task_id: TASK-103
ordinal: 242000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Implement the post-ES2025 features the engine currently excludes (see native/engine/crates/gable-js-test262/post-es2025-features.txt): Array.fromAsync, Error.isError, Math.sumPrecise, Uint8Array base64 and hex, JSON.parse source text access, iterator sequencing, Atomics.pause, upsert (Map getOrInsert), iterator chunking, iterator includes, joint iteration, and explicit resource management (using, DisposableStack). Temporal is separate. Model: Opus.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Each listed feature passes its test262 tests at the pinned commit, is removed from post-es2025-features.txt, and the gable and --precompile baselines show no regressions
- [ ] #2 Features still at Stage 3 are skipped with a recorded reason; the list is reviewed when the test262 pin moves
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Model assignment (maintainer, 2026-11-08): split by difficulty. Sonnet: Error.isError, Atomics.pause, Array.fromAsync, upsert (Map getOrInsert), iterator sequencing, iterator chunking, iterator includes, joint iteration, Uint8Array base64 and hex (self-contained built-ins, test262 is the oracle). Opus: Math.sumPrecise (exact-arithmetic algorithm), JSON.parse source text access and explicit resource management (they touch the parser or compiler).
<!-- SECTION:NOTES:END -->
