---
id: TASK-103.36
title: >-
  JIT: a directly called function that leaves with a leftover pc is read by its
  caller as its own continuation (latent)
status: Done
assignee:
  - '@claude'
created_date: '2026-11-13T22:38:00Z'
updated_date: '2026-11-14T04:46:00Z'
labels:
  - bug
  - jit
dependencies: []
references:
  - docs/jit-review-2026-11.md
  - crates/agnijs-js-jit/src/codegen.rs
  - crates/agnijs-js-jit/src/exec.rs
parent_task_id: TASK-103
priority: low
type: bug
ordinal: 301000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the JIT soundness review of 2026-11 (`docs/jit-review-2026-11.md`). Severity: low. **Latent: not reproduced from JavaScript.** Filed because the invariant that keeps it unreachable is held by code far from the place that relies on it.

## The hazard

Machine code answers its caller with a `u32`: an exit code at or above `EXIT_BASE` (`exec.rs`), or a bytecode pc below it, meaning "the runtime continued this frame at a pc compiled code has no successor block for". For code that keeps nothing in machine state (`CodeInfo::direct`), `exec::call_out` passes such a pc through untouched, and `Lower::continue_at` returns it (`codegen.rs`).

A function called directly through the call thunk (`Lower::thunk`) returns that word to the thunk. The thunk treats everything but `EXIT_RETURN` as "the callee left compiled code": it calls `left_trampoline`, which returns the word unchanged, and hands it to the **caller's** call site. The call site runs `continue_at(r, pc, &[pc + 1])`, which compares the word with the caller's own `pc + 1`.

So a callee that leaves with a leftover pc equal to the caller's `pc + 1` would make the caller go on at the instruction after the call as if the call had returned, while the callee's frame is still on top of `Vm::frames`. Every later call out would then run the caller's pcs against the callee's frame, and the caller's `Return` would pop the wrong frame: wrong results at least, and code after the call running twice.

## Why it is unreachable today

Read at commit fea680f:

- every `Flow::Next` of `Vm::step` leaves `frame.pc` at `pc + 1` or at the instruction's branch target, and `analysis::branch_offset` lists every instruction that uses `jump!` (checked one by one against `interp/step.rs`), so the pc is always one of the point's successors;
- a throw caught in the same frame does not come back as `Resume::Next`: `JitRt::after` turns every `Err` into `Exit::Fault`, and the interpreter routes it;
- the bytecode loader refuses a block that can run past its last instruction (`serialize.rs`, "code block can run past its last instruction"), so the "successor is the end of the block" case of `continue_at` cannot occur.

Nothing in `thunk` or `left_trampoline` checks any of this. An instruction added later whose runtime path continues at another pc, or a change that lets a handled throw resume as `Next` (the design document describes exactly that case for code with machine state), would turn this into a miscompilation with no test failing near the cause. A differential run of 40 callers with the call placed at each pc from 3 to 42 over a callee with a `try`/`catch` showed no divergence, as expected from the above.

## Suggested direction

Make the word unambiguous where it crosses a frame: in the thunk's "left" path (or in `left_trampoline`), turn any value below `EXIT_BASE` into `EXIT_INTERP` before it reaches the caller's `continue_at`. The interpreter frames are complete at that point, so `Exit::Interp` is always correct.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A word below EXIT_BASE returned by a directly called function never reaches the calling site as a pc to continue at
- [x] #2 A test with a toy runtime whose callee answers a non-successor pc shows the caller leaving for the interpreter with both frames intact
- [x] #3 docs/design/jit-deopt-and-gc-maps.md section 9 states what a directly called function may answer
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red test with a toy runtime that describes a frame stack (tests/calls.rs): a callee called directly whose call out is answered with the caller's own pc + 1; the caller must leave for the interpreter with both frames intact.
2. exec.rs: the word a directly called function answers is translated where it crosses the frame, in the thunk's left path (left_trampoline): only the exit codes that mean the same to every frame pass; a pc of the callee's becomes EXIT_INTERP.
3. State the contract where each side relies on it: the exit codes, compile_thunk, Lower::thunk, and section 9 of docs/design/jit-deopt-and-gc-maps.md.
4. Check the other paths by which a word reaches a call site (returned, the runtime's call, the nested entry).
5. Review status column.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Red first, and no longer only latent in the JIT crate: the toy runtime of tests/calls.rs answers a directly called function's call out with the caller's own pc + 1, and before the fix the caller ran on to its `Return` (`Exit::Return`) with the callee's frame still on the stack. Still unreachable from JavaScript for the reasons in the description.

Fix: `exec::left_code`, used by `left_trampoline`: EXIT_FRAME, EXIT_FAULT, EXIT_DONE, EXIT_INTERP and EXIT_PANIC pass, anything else (a pc, an undefined code, a return) is EXIT_INTERP. A debug assertion says the thunk never hands a return over. The other ways a word reaches a call site were checked: the thunk's own pc + 1 after a pop, `Runtime::returned` and `Runtime::call` (both called with the caller's context, so the pc they answer is the caller's), and the nested entry, which goes through `enter` and a typed `Exit`. I did not move the translation into the callee's machine code (so that compiled code never returns a pc at all): it would add code at every call out for something the frame boundary already settles.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
A word a directly called function answers no longer reaches the calling site as a pc. The thunk's 'left' path translates it (`exec::left_code`): the exit codes that mean the same to every frame pass, a pc of the callee's becomes EXIT_INTERP.

Verified: `a_callee_that_leaves_at_a_pc_of_its_own_makes_the_caller_leave_for_the_interpreter` (tests/calls.rs, red before: the caller returned) shows the caller leaving for the interpreter with both frames intact; `exec::tests` checks `left_code` and runs under Miri. The contract is in docs/design/jit-deopt-and-gc-maps.md, section 9.6, and at the exit codes, `compile_thunk` and `continue_at`.
<!-- SECTION:FINAL_SUMMARY:END -->
