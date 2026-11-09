---
id: TASK-103.17
title: Stack traces and source maps
status: To Do
assignee: []
created_date: '2026-11-09T05:17:00Z'
labels:
  - needs-triage
dependencies: []
parent_task_id: TASK-103
ordinal: 273000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Complete stack trace support: Error.stack with accurate positions, Error.captureStackTrace and Error.stackTraceLimit, async stack traces across await, and source-map support so traces and the debugger show original positions for code that was compiled or transformed (including TypeScript through TASK-103.13). The host supplies source maps through the module loading interface. Model: Opus.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Error.stack format and Error.captureStackTrace follow the V8 convention closely enough for common tooling, with tests
- [ ] #2 Async stack traces cross await, and a source map applied by the host changes the positions in Error.stack and in the debugger
<!-- AC:END -->
