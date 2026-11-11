---
id: TASK-103.14
title: 'Optional crate: Node-style module resolver'
status: Done
assignee: []
created_date: '2026-11-09T02:28:00Z'
updated_date: '2026-11-11T07:23:00Z'
labels:
  - needs-triage
dependencies: []
parent_task_id: TASK-103
ordinal: 249000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
A Node-style module resolver and compatibility layer (package.json exports and imports, node_modules lookup, extension resolution) as a separate optional crate on the host module-loading interface, split out of the former TASK-103.13. Decide first whether it belongs in AgniJS or in embedder projects, and keep the engine core free of it. Model: Opus.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A short ADR decides scope and ownership of a module resolver
- [x] #2 If in scope: a separate crate with tests against a documented fixture set, and the engine core gains no dependency on it
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Model assignment (maintainer, 2026-11-08): Sonnet; the Node resolution algorithm is fully specified and a fixture suite checks the result.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
ADR 0023 puts the resolver in scope as crates/agnijs-js-resolve (optional, no engine crate depends on it, it depends on none). 17 fixture tests plus 2 host-driven tests via Engine::load_module pass. Commit 4d26a10.
<!-- SECTION:FINAL_SUMMARY:END -->
