---
id: TASK-100.18
title: >-
  Compiler: emit fewer accumulator moves (register operands, fused
  compare-and-branch)
status: To Do
assignee: []
created_date: '2026-11-09T03:18:00Z'
updated_date: '2026-11-09T05:53:00Z'
labels:
  - ready-for-agent
dependencies:
  - TASK-101.1
documentation:
  - docs/js-engine-performance-findings.md
parent_task_id: TASK-100
priority: high
type: enhancement
ordinal: 252000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Star and Ldar are 50 to 60% of all executed instructions. Opcode histograms: loop-arith runs 69.0 M instructions for 3 M iterations of `s = (s + i * 7) % 1000003; i++` (23 per iteration; Star 30.4%, Ldar 26.1%), prop-monomorphic 35 per inner iteration (Star 37.1%, Ldar 20.0%), calls-recursive 20 per fib call (Star 32.5%, Ldar 17.5%). The compiler also emits ToNumeric for a postfix `i++` whose old value is unused (4.3% of loop-arith's instructions). Every instruction pays the dispatch cost (TASK-100.17), so the count multiplies it.

Reproduction: count instructions for loop-arith with a temporary opcode counter (the findings doc describes the one used), or compare bytecode dumps of the loop body before and after.

Benchmarks to prove it: loop-arith, loop-array-index, prop-monomorphic, calls-recursive (micro) and signal-churn, rxjs-operators (Gable).

Possible directions (the implementer decides): register-operand or immediate forms of the hot binary operators and comparisons, fused compare-and-jump, dropping unused ToNumeric, and a peephole pass over Star/Ldar pairs. A change to the instruction set changes the serialized bytecode, which needs a format tag bump and an ADR 0010 addendum; keep Instr small (12 bytes today).

Findings: docs/js-engine-performance-findings.md, finding 3. Keep the engine crates Gable-agnostic (ADR 0016); the agnijs-* rename is TASK-100.12, so use the current crate names. Measure before and after with the same binaries and machine; micro-benchmarks via `native/engine/target/release/bench.exe run --engine gable --engine quickjs` and `bench compare`, Gable benchmarks via `node native/engine-harness/js/run-bench.mjs --engine gable --engine quickjs`. The sampling profiler is native/tools/winprof (see the findings doc, Method).

Model assignment (maintainer, 2026-11-08, via TASK-100.14): Fable.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 loop-arith executes at least 35% fewer instructions per iteration, and calls-recursive at least 25% fewer per call
- [ ] #2 An unused postfix update no longer emits ToNumeric, with a compiler test
- [ ] #3 If the bytecode format changes, the serialization tag is bumped, old payloads are rejected as a format mismatch, and ADR 0010 has an addendum
- [ ] #4 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [ ] #5 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
JIT-readiness (ADR 0019, TASK-101.1): J2, J3, J4 apply. New forms (register-operand, immediate, fused compare-and-branch) must be atomic for deoptimization: every observable effect (ToPrimitive/valueOf, getters, calls) comes before a single result write to acc or one register, or a single branch decision, so a frame can resume at pc or pc+1 (J3). Keep instructions fixed-size and linearly decodable, pc an instruction index (J2). If a form needs feedback, give it a compiler-allocated side-table slot index; never widen the instruction with feedback (J4).
<!-- SECTION:NOTES:END -->
