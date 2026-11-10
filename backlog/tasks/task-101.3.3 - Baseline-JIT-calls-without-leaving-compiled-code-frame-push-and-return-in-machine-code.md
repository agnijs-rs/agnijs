---
id: TASK-101.3.3
title: >-
  Baseline JIT: calls without leaving compiled code (frame push and return in
  machine code)
status: To Do
assignee: []
created_date: '2026-11-10T11:19:00Z'
labels:
  - needs-triage
dependencies: []
references:
  - docs/adr/0020-baseline-jit-on-cranelift-codegen-with-no-speculation.md
  - docs/design/jit-deopt-and-gc-maps.md
parent_task_id: TASK-101.3
ordinal: 287000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Measured in TASK-101.3.2 (ADR 0020 addendum): inline IC hits make property-heavy loops 1.3 to 1.4 times faster, but every call still leaves compiled code (Exit::Frame), goes through Vm::jit_run_slow and its J7 polls, and the return re-enters the caller with the frame reloaded from memory. That round trip costs about 30 ns, against about 11 ns for a call out, so closures, calls-recursive and method-calls lose 25 to 33% when forced, and Gable's benches (rxjs, signals, templates) lose 16 to 38%. Mode on can only leave such code to the interpreter, where it breaks even. Two options to weigh: (a) without native recursion, push the callee's interpreter frame in machine code (same frame format, J1) and hand over through a tight trampoline that enters the callee's compiled code without the slow-path checks, with the return path mirrored; this needs no deoptimization or GC maps if nothing stays in machine registers across the call. (b) Direct calls between compiled functions on the native stack, which need TASK-101.3.1 (deopt and GC maps) and the native-depth and birth-serial accounting reworked. Also worth weighing: compile time is about 25 us per bytecode instruction once IC paths are expanded inline, and out-of-line IC stubs would cut it.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 closures, calls-recursive and method-calls are faster with AGNIJS_JIT=force than interpreted, measured with agnijs-js-bench
- [ ] #2 Gable's rxjs and signal benches are no slower with AGNIJS_JIT=force than interpreted
- [ ] #3 jit_differential, the VM suite and test262 pass with AGNIJS_JIT=force, including under GC stress
<!-- AC:END -->
