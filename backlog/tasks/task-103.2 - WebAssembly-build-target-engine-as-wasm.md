---
id: TASK-103.2
title: WebAssembly build target (engine as wasm)
status: To Do
assignee: []
created_date: '2026-11-09T02:14:00Z'
labels:
  - ready-for-agent
dependencies: []
parent_task_id: TASK-103
ordinal: 237000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Build the engine for wasm32 so it runs in browsers and WASM hosts (WASI and wasm32-unknown-unknown), with a documented demo. Pure Rust and no JIT make this possible. Cover the host trait over a wasm host, timers and jobs without threads, stack depth limits on small wasm stacks, and size. Model: Opus.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 The engine workspace builds for wasm32 (CI job) and runs the test262 baseline subset and the host-driven tests under a wasm runtime, with documented gaps
- [ ] #2 A browser demo (REPL or playground) runs the engine as wasm, and its download size is recorded
- [ ] #3 No platform-specific engine code beyond cfg-gated host glue, per spec section 2
<!-- AC:END -->
