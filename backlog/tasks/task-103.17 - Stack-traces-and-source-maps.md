---
id: TASK-103.17
title: Stack traces and source maps
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-09T05:17:00Z'
updated_date: '2026-11-11T07:27:00Z'
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

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Error.prototype.stack accessor (error-stack-accessor proposal, test262 feature), captured frames in the [[ErrorData]] slot at creation, formatted lazily in the V8 format; Error.stackTraceLimit; Error.captureStackTrace; Error.prepareStackTrace with CallSite objects.
2. Async frames: walk the await chain from the bottom async frame (zero-cost: only at capture).
3. Host source maps: Engine::set_source_map and ModuleSource::SourceWithMap; Error.stack, Exception.stack and the debugger (ScriptInfo, CDP sourceMapURL and exception text) map positions; hardened bounded decoder with a fuzz test.
4. C API entry points (ABI 1.2), docs, ADR addendum; verify test262 (default, precompile, snapshot, intl402), GC stress, jit, no-intl, bench.
<!-- SECTION:PLAN:END -->
