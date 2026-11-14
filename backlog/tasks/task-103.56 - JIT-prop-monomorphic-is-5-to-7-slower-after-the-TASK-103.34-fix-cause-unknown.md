---
id: TASK-103.56
title: >-
  JIT: prop-monomorphic is 5 to 7% slower after the TASK-103.34 fix, cause
  unknown
status: To Do
assignee: []
created_date: '2026-11-14T04:29:00Z'
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
- [ ] #1 The cause is established: a difference in the inner loop's machine code, or code layout shown by a measurement that controls for it
- [ ] #2 prop-monomorphic with the JIT on and forced is within noise of the commit before the TASK-103.34 fix, or the task records why the cost stays
- [ ] #3 The invariant of TASK-103.34 still holds: tests/jit_deopt.rs, handoff::tests and the differential tests pass, and no register is boxed from bits
<!-- AC:END -->
