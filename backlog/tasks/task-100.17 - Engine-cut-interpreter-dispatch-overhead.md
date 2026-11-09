---
id: TASK-100.17
title: 'Engine: cut interpreter dispatch overhead'
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
ordinal: 251000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Interpreter dispatch costs more than the work it dispatches. `Vm::step` is an out-of-line call per instruction whose prologue pushes eight registers, reserves 0x4D8 bytes of stack, saves xmm6 and sets up an EH state. The run loop (`interp/mod.rs`) tests four flags (`heap.stress`, `gc_requested`, `allocated_since_gc >= gc_threshold`, `debug_hot`) before every instruction, reads and writes `self.frames[fi].pc` with bounds checks, and `Flow::Reload` clones the `Rc<CodeBlock>` on every call and return. The accumulator lives in `self.frames[fi].acc` and registers in `self.regs[base + r]`, all bounds-checked. In loop-arith, the step prologue and epilogue are about 22% of samples and the run loop about 25%. `step` plus `run` self time is 26 to 55% of every Gable benchmark (signal-churn 55%, rxjs-operators 47%).

Reproduction: an empty `for (let i = 0; i < 1e6; i++) s += 1` loop in a module takes 129 ms against 37 ms on QuickJS-NG.

Benchmarks to prove it: loop-arith, prop-monomorphic, calls-recursive, method-calls (micro) and signal-churn, rxjs-operators, rxjs-subject-fanout (Gable).

This is independent of the bytecode-shape ticket (TASK-100.18), but the gains multiply. Keep the debugger hooks (`debug_hot`), GC safe points, the interrupt poll and the memory limit working; they can move behind one attention flag.

Findings: docs/js-engine-performance-findings.md, finding 2. Keep the engine crates Gable-agnostic (ADR 0016); the agnijs-* rename is TASK-100.12, so use the current crate names. Measure before and after with the same binaries and machine; micro-benchmarks via `native/engine/target/release/bench.exe run --engine gable --engine quickjs` and `bench compare`, Gable benchmarks via `node native/engine-harness/js/run-bench.mjs --engine gable --engine quickjs`. The sampling profiler is native/tools/winprof (see the findings doc, Method).

Model assignment (maintainer, 2026-11-08, via TASK-100.14): Fable.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 pc, accumulator and the register window are kept in locals of the dispatch loop and written back only where frames are observed (calls, returns, throws, safe points, debugger stops)
- [ ] #2 The per-instruction flag tests are replaced by a single check, and debugger stepping, breakpoints, interrupts, the memory limit and GC stress still work (their existing tests pass)
- [ ] #3 loop-arith and calls-recursive are at least 1.3x faster than at the start of the task
- [ ] #4 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [ ] #5 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
JIT-readiness (ADR 0019, TASK-101.1): J1, J2, J3, J7 apply. Locals for pc/acc/window are fine, but write them back before any call, throw, safe point, debugger stop or suspension (J1), and address registers by base index since the register file can reallocate. pc stays an instruction index with O(1) mapping if you change encoding (J2). When inlining opcode bodies, keep each opcode's generic semantics callable as a Vm function a JIT can call (J3). Build the attention word as one word at a fixed Vm field, polled at function entry, back-edges and after calls return (J7; TASK-100.33 covers the missing entry poll). No unsafe in agnijs-js-vm (J10, TASK-100.32): get bounds-check relief from verified frame sizes and slice windows; raise it with the maintainer if unchecked access proves necessary.
<!-- SECTION:NOTES:END -->
