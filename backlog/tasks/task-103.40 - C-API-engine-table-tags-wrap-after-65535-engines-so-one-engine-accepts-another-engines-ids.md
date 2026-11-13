---
id: TASK-103.40
title: >-
  C API: engine table tags wrap after 65,535 engines, so one engine accepts
  another engine's ids
status: To Do
assignee: []
created_date: '2026-11-13T22:48:00Z'
labels:
  - bug
  - security
dependencies: []
parent_task_id: TASK-103
priority: low
ordinal: 305000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the embedder-surface review of 2026-11 (docs/surface-review-2026-11.md), at commit fea680f.

Severity: low, security. The C API stays memory-safe, but a documented guarantee breaks. docs/c-api.md and docs/security-model.md say a stale or foreign id is "refused, never dereferenced". After 65,535 engines, an id of one engine is accepted by another and resolves to that engine's own value. An engine-per-request server that holds a stale id (a use-after-free bug in the embedder) would read, overwrite or free another tenant's value instead of getting AGNIJS_ERR_WRONG_ENGINE.

Affected code: crates/agnijs-capi/src/registry.rs, `next_engine_id`: `tag = ((id - 1) % 0xFFFF) + 1`. Engine ids are unique per process, but the 16-bit table tag that `table.rs::check`, `EngineState::realm` and `EngineState::class` compare repeats every 65,535 engines. A fresh engine's table also starts with the same generations, so the generation check does not separate them either.

Minimal repro (integration test in crates/agnijs-capi/tests):
1. Create engine A, make a number value `va` (1111), and keep A alive.
2. Create and free engines until one has the same tag as A (65,535 engines; 314 s in a debug build). Keep that one as B and make a number (2222) in it.
3. `agnijs_value_get_number(B, va)` returns AGNIJS_OK and 2222. `agnijs_value_free(B, va)` returns AGNIJS_OK and frees B's value. `agnijs_engine_main_realm` returns the same id (0x1000000000001) for A and B.

Command: `cargo test -p agnijs-capi --test <name> -- --nocapture`.

Expected: AGNIJS_ERR_WRONG_ENGINE for an id of another live or freed engine, as the docs promise.
Actual: B accepts A's id as its own.

Suggested fix (one of these):
- Widen the engine part of the id. For example, spend more bits on the tag and fewer on the slot index (2^32 slots is far beyond any table the memory limit allows).
- Keep a per-process set of live tags and skip tags in use when allocating. This closes the live-live case but not stale ids of freed engines.
- Document the limit precisely if neither is acceptable.

Test that should catch it: a test that allocates tags past the wrap and checks that foreign ids are refused. A test-only hook that starts `NEXT_ENGINE` near 0xFFFF would avoid creating 65,535 engines.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 An id of another live engine is refused with AGNIJS_ERR_WRONG_ENGINE however many engines the process has created
- [ ] #2 The behaviour for ids of freed engines is either refused or precisely documented in docs/c-api.md and docs/security-model.md
- [ ] #3 A test covers the wrap without creating 65,535 full engines
<!-- AC:END -->
