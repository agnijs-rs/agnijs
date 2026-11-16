---
id: TASK-103.13.2
title: 'TypeScript: original positions end to end, the CLI feature and an example'
status: To Do
assignee:
  - '@claude'
created_date: '2026-11-16T16:50:00Z'
labels:
  - typescript
dependencies: []
parent_task_id: TASK-103.13
ordinal: 348000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Transpiled code must report .ts positions everywhere the engine reports positions, and the CLI should run .ts files directly when built with the feature.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Error.stack and Exception::stack of transpiled modules and scripts show original .ts lines and columns
- [ ] #2 A CDP client sets a breakpoint through the transpiled code's source map and pauses at the original .ts line
- [ ] #3 agnijs built with the typescript feature runs .ts, .mts, .cts and .tsx files and prints TypeScript diagnostics with positions; default builds are unchanged
- [ ] #4 An example embeds the crate in a Host::load_module and runs in CI
<!-- AC:END -->
