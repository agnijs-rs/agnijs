---
id: TASK-109
title: Consider splitting the largest debugger and bytecode source files
status: To Do
assignee: []
created_date: '2026-11-09T05:40:00Z'
labels:
  - ready-for-agent
  - sonnet
dependencies: []
priority: low
ordinal: 280000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
A code review flagged three files as large enough to slow navigation and review: crates/agnijs-js-vm/src/debug.rs (about 1180 lines: script registry, dispatch-loop hook, pause handling, frame and scope inspection, eval-in-frame and the Debuggee impl), crates/agnijs-js-compiler/src/serialize.rs (about 1160 lines: encoder, decoder and verifier for agnijs-bc-3) and crates/agnijs-js-cdp/src/lib.rs (about 950 lines: the Session with both the Runtime and Debugger domains). Nothing is broken; this is a structure smell. Decide per file whether a split into submodules along those seams makes it easier to read, and leave a file alone if not. Low priority, mechanical, suited to a Sonnet agent. Pure moves only: no behaviour change, no public API change.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Each of the three files is either split into submodules along its natural seams or left as is with a one-line reason in the task notes
- [ ] #2 No public item of agnijs-js-vm, agnijs-js-compiler or agnijs-js-cdp changes path or signature
- [ ] #3 cargo test for the three crates and the boundary test pass; the bytecode-load and interpreter benchmarks show no change
<!-- AC:END -->
