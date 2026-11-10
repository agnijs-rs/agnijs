---
id: TASK-101.3.2
title: 'Baseline JIT: inline IC hit paths in machine code, and MAP_JIT on macOS'
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-10T08:41:00Z'
updated_date: '2026-11-10T09:26:00Z'
labels:
  - needs-triage
dependencies: []
parent_task_id: TASK-101.3
ordinal: 286000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Measured in ADR 0020: compiled code is about 2.9x faster on blocks that run inline, but 16-40% slower when forced on property- and call-heavy blocks, because every property access calls out to the runtime. Mode on therefore leaves those blocks to the interpreter. Inline IC hit paths (object header at the ADR 0019 J5 offsets, IC side table) with a full miss path to Runtime::slow stay within the no-speculation design and need no deopt. Separately, the macOS row of ADR 0019 (MAP_JIT plus per-thread write protection) is not met yet; macOS falls back to the interpreter.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Named property loads and stores that hit a monomorphic IC run without calling out of compiled code
- [ ] #2 prop-monomorphic and method-calls are faster with AGNIJS_JIT=on than interpreted, measured with agnijs-js-bench
- [ ] #3 On macOS with the allow-jit entitlement, the probe succeeds using MAP_JIT and pthread_jit_write_protect_np
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Layout plumbing: the VM describes its object arena, object header, slots, proto, array length, dense elements and IC table to the JIT (JitLayout), computed in safe code at start-up with probes; any failed check disables inline IC paths (they call Runtime::slow as before).
2. Ctx gains the arena base, proto epoch and IC table, refreshed after every call out (never held across one).
3. Inline GetNamed (own data inline/overflow slots, proto data, array length; up to 4 entries, the interpreter's order) and SetNamed (own data), miss -> Runtime::slow at the same pc.
4. Inline GetKeyed/SetKeyed on dense elements.
5. JitStats.calls_out; profitability counts inline IC sites as inline.
6. Tests at the seams: jit_differential (interpreter vs on/force, plus calls_out stats), bench jit_workloads checksums, GC stress forced every safe point.
7. macOS MAP_JIT + pthread_jit_write_protect_np in exec.rs, cfg-gated.
8. CI: --features jit with AGNIJS_JIT=force on Linux/Windows/macOS (differential, VM suite, test262 subset).
9. Re-measure (alternating binaries, median of 5), tune mode on; gates; ADR/notes.
<!-- SECTION:PLAN:END -->
