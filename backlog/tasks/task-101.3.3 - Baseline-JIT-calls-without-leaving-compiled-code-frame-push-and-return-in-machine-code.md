---
id: TASK-101.3.3
title: >-
  Baseline JIT: calls without leaving compiled code (frame push and return in
  machine code)
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-10T11:19:00Z'
updated_date: '2026-11-10T12:45:00Z'
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

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Nested entry without native-stack speculation: when an instruction run by Runtime::slow (a call, construct, getter, setter) pushes exactly one bytecode frame whose block has compiled code, the runtime enters the callee's compiled code from inside the call out (bounded nesting depth), pushing the frame the interpreter's way (push_function_frame), with the function-entry poll; on the callee's Return it pops the frame (do_return), polls, and resumes the caller's compiled code at pc+1 directly (Resume::Next), with no exit to Vm::jit_run. Invariants I1-I3 hold: the caller has flushed everything to its interpreter frame, so no GC map is needed and nothing deoptimizes; any other exit of the callee (Frame, Fault, Interp, Done) unwinds every nested machine frame back to Vm::jit_run, whose frames are already interpreter-valid.
2. Debugger attached: never nest (J9); attention word: service the safe point as jit_run_slow does.
3. Tests (jit_differential): direct-call stat under force for fib/closures/methods; exceptions thrown in a nested callee caught by a compiled caller and unwinding several compiled frames; deep recursion past the nesting cap; GC stress; interrupts; debugger.
4. Cut the per-call cost (dedicated call path, skip redundant checks), measure per workload, alternating binaries median of 5.
5. Mode on: let loop-free callees compile when called from compiled code if profitable; retune ENTRY_SHARE/judge so mode on never loses.
6. Feature-off interpreter: same-path builds of main vs head on prop-write-alloc and promise-chain; fix if real.
7. Gates: test262 (default, --precompile, intl402, RegExp x3) off/on/force, gc-stress subset, Gable suites; ADR 0020 addendum; notes.
<!-- SECTION:PLAN:END -->
