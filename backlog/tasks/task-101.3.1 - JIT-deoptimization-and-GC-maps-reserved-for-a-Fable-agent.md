---
id: TASK-101.3.1
title: JIT deoptimization and GC maps (reserved for a Fable agent)
status: To Do
assignee: []
created_date: '2026-11-10T08:41:00Z'
labels:
  - needs-triage
dependencies: []
references:
  - docs/design/jit-deopt-and-gc-maps.md
  - docs/adr/0020-baseline-jit-on-cranelift-codegen-with-no-speculation.md
parent_task_id: TASK-101.3
ordinal: 285000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The baseline JIT (ADR 0020) never speculates and never holds a JS value outside the frame across a call out, so it needs neither side exits nor GC maps. Any optimisation that keeps state in machine registers, drops a generic fallback, or calls directly between compiled functions needs both first. The interfaces are stubbed in crates/agnijs-js-jit/src/handoff.rs and specified in docs/design/jit-deopt-and-gc-maps.md (frame layout, side-exit state, safe points, root finding without stack scanning, the birth-serial bound). Per the project's model assignment this is Fable work.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 The four ignored contract tests in agnijs-js-jit/src/handoff.rs pass un-ignored
- [ ] #2 jit_differential passes with an optimisation that needs deopt and GC maps enabled, including under GC stress
- [ ] #3 test262 passes at the baselines with AGNIJS_JIT=force
- [ ] #4 No unsafe outside agnijs-js-jit's exec-style module and no native stack scanning
<!-- AC:END -->
