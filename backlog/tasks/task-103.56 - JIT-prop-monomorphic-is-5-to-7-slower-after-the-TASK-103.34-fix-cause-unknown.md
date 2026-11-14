---
id: TASK-103.56
title: >-
  JIT: prop-monomorphic is 5 to 7% slower after the TASK-103.34 fix, cause
  unknown
status: Done
assignee:
  - '@claude'
created_date: '2026-11-14T04:29:00Z'
updated_date: '2026-11-14T15:54:00Z'
labels:
  - bug
  - jit
  - performance
dependencies: []
references:
  - docs/jit-review-2026-11.md
  - crates/agnijs-js-jit/src/codegen.rs
parent_task_id: TASK-103
priority: low
type: bug
ordinal: 321000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found while measuring the fix of TASK-103.34 (`docs/jit-review-2026-11.md`, section "After the fixes"). The fix was pushed with this cost open, by decision: a soundness fix was not held for one micro-benchmark.

## What is slower

The `prop-monomorphic` workload of `agnijs-js-bench` with the `jit` feature, in modes `on` and `force`. Release builds of the starting commit (`e7df99b`) and of the fix from one checkout path, five alternating rounds, each figure the median over the rounds of each run's median sample (5 iterations after 1 warm-up):

- `AGNIJS_JIT=on`: 25.52 ms before, 26.98 ms after (1.057)
- `AGNIJS_JIT=force`: 26.10 ms before, 27.44 ms after (1.051)

Four later runs of mode `on`, each of a different experimental build, gave 1.069, 1.050, 1.050 and 1.074, with spreads of 0.4 to 2.4%. No other workload was slower in every build: loop-arith, loop-array-index and prop-polymorphic moved by 4% either way when only code on the cold call-out path changed, which is this machine's layout noise for JIT code. The geomeans did not move (on 0.990 over 26 workloads, force 1.003 over 15, feature off 1.005).

The loop:

```js
function bench() {
  var s = 0, rounds = scaled(1000);
  for (var r = 0; r < rounds; r++) {
    for (var i = 0; i < pts.length; i++) { var p = pts[i]; s = (s + p.x + p.y) | 0; }
  }
  return s;
}
```

## What is known

- The tiering is the same. With `AGNIJS_JIT_STATS=1` in mode `on`, before and after: 2 blocks compiled, 2 with guesses, 1 deopt, 1474 call outs, 5,997,605 back-edges. Only the machine code differs: 56,752 bytes before, 57,984 after.
- Not the tag word a speculated register now carries beside its payload (`Repr::Number { bits, tag }`): with every tag forced to the constant (not sound, an experiment) the ratio stayed at 1.050.
- Not the one shared constant the tag is defined with (`Lower::number_tag`): with a local constant at every read as well, 1.050.
- Not the wider spill at a point (tag and payload, where it was the payload alone): with the old 8-byte spill restored on top of both, 1.074.

So the three hot-path suspects are ruled out, and none of the experiments recovered it. What is left of the fix in the code generator is a constant defined in the entry block and what the points list. Whether the cost is code layout, like the other loop workloads' movement, was not established: nobody has looked at the machine code of the two builds.

## Where to start

Disassemble `bench`'s inner loop from both builds (the JIT has no dump option yet; `Lowered::bytes` in `Compiled::generate` is the place to write one out) and compare the hot path instruction by instruction. If it is identical, measure with the loop's alignment controlled before changing anything.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 The cause is established: a difference in the inner loop's machine code, or code layout shown by a measurement that controls for it
- [x] #2 prop-monomorphic with the JIT on and forced is within noise of the commit before the TASK-103.34 fix, or the task records why the cost stays
- [x] #3 The invariant of TASK-103.34 still holds: tests/jit_deopt.rs, handoff::tests and the differential tests pass, and no register is boxed from bits
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Build bench at e7df99b and main from one checkout path; reproduce the 5 to 7% with interleaved rounds (on and force).
2. Add a machine-code dump (env switch) and disassemble bench's inner loop from both builds; compare instruction by instruction.
3. If identical, control the loop's alignment and re-measure; else fix the difference keeping the TASK-103.34 invariant (a register is never boxed from bits).
4. Gates: jit_deopt, handoff tests, differential tests, jit-fuzz default run.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Cause found 2026-11-14 with AGNIJS_JIT_DUMP (new) and llvm-objdump on bench's second compile, 8af8d4a against main. The inner loop's instruction selection is the same (516 against 522 instructions on the traced hot path). The register allocation differs: before the fix the shared-state pointer (the function's second argument, read for the arena base, the epoch and the interrupt counter) stayed in r14 through the loop; after it, it lives in a stack slot and is reloaded 12 times per iteration (3 before), 34 stack loads per iteration against 27. Nothing in the loop is a tag word: every speculated register's tag is the constant there. The fix added values elsewhere in the function and the allocator (regalloc2: whole-function ranges have the lowest priority per instruction) split the pointer's range differently. Every cached register was live across every call out of the block, so they all competed for the seven registers calls preserve.

Fix: cached registers are read back after every call out (from the register file or the spill slot the point left them in) instead of being held across it; the point and what it lists are unchanged. 9 rounds over 8af8d4a: prop-monomorphic on 1.051 to 1.024 (0.986 to 1.031), force 1.058 to 1.032; prop-polymorphic 1.052 to 0.986; loop-array-index 0.965 to 0.897; loop-arith 1.006 to 1.019 (its loop is two instructions shorter; alignment). The shared-state pointer is still on the stack in prop-monomorphic's loop (15 reloads); reading the context pointers back after calls as well was tried and was worse (1.085), so the remaining 2 to 3% stays, inside the 4% layout noise.

AC 3: tests/jit_deopt.rs, handoff::tests, jit_differential and the whole VM suite forced pass; jit_deopt and jit_inline_cache also under AGNIJS_GC_STRESS=1; jit-fuzz default run 541 programs and the 10 regressions, 0 divergences. Location::SpillNumber is not back: a point still lists values only.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Cause established by dumping and disassembling the inner loop from both builds (new AGNIJS_JIT_DUMP): identical instruction selection, different register allocation. The shared-state pointer moved from r14 to a stack slot and was reloaded 12 times an iteration. Underneath, every cached register was live across every call out, so they all competed for the registers a call preserves. Cached registers are now read back after each call out (cold path), which leaves only the two context pointers live across calls; the TASK-103.34 invariant is untouched. prop-monomorphic is at 1.024 (on) and 1.032 (force) of the commit before the fix, within layout noise; prop-polymorphic and loop-array-index gained 6 to 7%. Not back to 1.00: the allocator still spills the pointer in that loop, recorded with what was tried. Commit a66d346.
<!-- SECTION:FINAL_SUMMARY:END -->
