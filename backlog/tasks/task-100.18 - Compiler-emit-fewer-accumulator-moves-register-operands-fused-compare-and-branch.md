---
id: TASK-100.18
title: >-
  Compiler: emit fewer accumulator moves (register operands, fused
  compare-and-branch)
status: Done
assignee:
  - '@claude-fable'
created_date: '2026-11-09T03:18:00Z'
updated_date: '2026-11-09T10:34:00Z'
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
- [x] #1 loop-arith executes at least 35% fewer instructions per iteration, and calls-recursive at least 25% fewer per call
- [x] #2 An unused postfix update no longer emits ToNumeric, with a compiler test
- [x] #3 If the bytecode format changes, the serialization tag is bumped, old payloads are rejected as a format mismatch, and ADR 0010 has an addendum
- [x] #4 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [x] #5 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red: baseline numbers with the origin/main bench.exe (loop-arith, loop-array-index, prop-monomorphic, calls-recursive, full micro set) and a feature-gated instruction counter (agnijs-js-vm feature instr-count, Vm::instr_count bumped once per dispatched instruction) with tests in tests/instr_count.rs that assert loop-arith <= 14 instructions per iteration (23 today) and fib <= 15 per call (20 today); compiler shape tests (unused postfix update emits no ToNumeric, loop test is one fused compare-and-branch, binary operators on register variables take register/immediate operands).
2. Bytecode (ADR 0008/0019 J2, J3): BinOp operand enum; value forms BinRR{op,a,b} (acc = a OP b), BinRI{op,a,imm} (acc = a OP imm), BinAR{op,b} (acc = acc OP b), BinAI{op,imm} (acc = acc OP imm); fused JumpIfCmpRR/JumpIfNotCmpRR{op,a,b,offset}, JumpIfCmpRI/JumpIfNotCmpRI{op,a,imm,offset}, JumpIfCmpRA/JumpIfNotCmpRA{op,a,offset} (a CMP acc). Instr stays 12 bytes. Each performs every conversion before its single acc write or branch decision.
3. Compiler: an operand lowering that uses a register-resident binding's own register (no Ldar/Star) when no TDZ check is needed and the other operand cannot write locals (syntactic check for assignment/update in the sibling expression); integer literals become immediates; condition contexts (if, while, for, do-while, ?:, with && / || / ! handled) emit fused compare-and-branch; postfix updates whose value is unused compile like prefix (no ToNumeric; Inc/Dec convert); member gets and plain sets address a register-resident object/key directly.
4. VM: generic semantics in Vm::step via one binary_op(op, l, r) / compare(op, l, r) helper (J3), Number fast paths inlined in Vm::run for the new forms.
5. Serializer (ADR 0010): table entries, BinOp wire encoding rejected when out of range, fused compare verified as a comparison operator; FORMAT agnijs-bc-5, agnijs-bc-4 rejected as a mismatch; ADR 0010 addendum.
6. Tests per new instruction (step and inlined paths, non-number operands, valueOf side effects and exceptions mid-instruction leaving acc/registers untouched, handler lookup at the fused pc), debugger stepping/line mapping over fused loop tests and breakpoints on fused if tests, serializer round trip.
7. Measure after alternating binaries; cargo test --workspace; debugger, cdp, targeted GC stress subset; test262 full, --precompile, intl402, RegExp suites once each; Gable suites and benchmarks through the path-patched harness; record numbers; finalize; commit; push.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
JIT-readiness (ADR 0019, TASK-101.1): J2, J3, J4 apply. New forms (register-operand, immediate, fused compare-and-branch) must be atomic for deoptimization: every observable effect (ToPrimitive/valueOf, getters, calls) comes before a single result write to acc or one register, or a single branch decision, so a frame can resume at pc or pc+1 (J3). Keep instructions fixed-size and linearly decodable, pc an instruction index (J2). If a form needs feedback, give it a compiler-allocated side-table slot index; never widen the instruction with feedback (J4).

Results (commit 05a85b0 on main, 2026-11-09). Root cause: every binary operator spilled its left operand to a temporary and reloaded the right through the accumulator, every loop/if test materialised a boolean before a separate conditional jump, and an unused postfix update still emitted ToNumeric; the compiler never used a register-held binding's own register as an operand.

Changes: BinOp enum and value forms BinRR/BinRI/BinAR/BinAI, fused JumpIf[Not]Cmp{RR,RI,RA,AI} (Not forms are true negations, so NaN is correct); Instr stays 12 bytes; every form performs all conversions before its single acc write or branch decision (ADR 0019 J3). Compiler operand lowering uses a register-held binding (no TDZ check) or a small integer literal in place when the sibling operand cannot write locals (conservative syntactic walk, may_write_locals); conditions in if/while/do-while/for/?: including !, && and || branch directly; unused postfix update compiles like prefix; member gets, plain member sets, callees and call arguments address register-held values directly. Immediates only on the right and relational operands never swapped, so ToPrimitive order is the spec's. VM: generic semantics in Vm::step via binary_op/compare, Number fast paths inlined in Vm::run. FORMAT agnijs-bc-4 -> agnijs-bc-5, verifier rejects a non-comparison operator in a fused jump, ADR 0010 addendum. Feature instr-count (Vm::instr_count) with tests/instr_count.rs pinned in CI.

Instructions dispatched (instr-count): loop-arith 23 -> 9 per iteration (-61%; bar 35%), calls-recursive 20 -> 13 per fib call (-35%; bar 25%), prop-monomorphic 35 -> 19.1 per inner iteration (-45%).

Micro-benchmarks, medians of 7 after 2 warmup, same machine, baseline binary built from c7fb4b8 and run twice around the new binary (before / before-rerun -> after): loop-arith 154.73 / 151.16 -> 105.61 ms (0.68-0.70); loop-array-index 220.28 / 218.48 -> 196.24 (0.89-0.90); prop-monomorphic 172.93 / 173.15 -> 149.25 (0.86); calls-recursive 85.35 / 85.30 -> 82.58 (0.97); prop-polymorphic 314.05 / 308.26 -> 291.44; prop-write-alloc 248.32 / 240.69 -> 238.73; string-concat 119.65 / 120.19 -> 121.68 (1.01-1.02); string-methods 57.70 / 57.12 -> 57.38; array-methods 456.23 / 456.25 -> 451.88; map-set 179.79 / 177.66 -> 177.00; closures 154.25 / 156.76 -> 153.35; method-calls 212.27 / 210.94 -> 210.22; regex 343.99 / 348.28 -> 348.70 (1.00-1.01); regex-tokenize 67.02 / 66.33 -> 65.22; regex-long-input 7.37 / 7.58 -> 7.30; json-stringify 177.55 / 178.34 -> 174.38; json-parse 130.70 / 131.03 -> 129.35; promise-chain 77.34 / 77.10 -> 74.70; async-await 79.60 / 77.16 -> 78.62 (0.99-1.02); compile-source 20.48 / 20.34 -> 19.95; bytecode-load 2.30 / 2.36 -> 2.11; engine-startup 0.65 / 0.67 -> 0.61. Worst new/old 1.02, inside run-to-run noise; no workload slower by more than 5%.

Gable benchmarks via the path-patched engine-harness (baseline = c7fb4b8 crates, after = this commit; medians): signal-churn 615.5 -> 555.2 ms (0.90); rxjs-operators 298.0 -> 300.3 (1.01); rxjs-higher-order 471.0 -> 457.3; rxjs-subject-fanout 223.1 -> 220.8; signal-create 140.5 -> 137.7; template-mount 108.1 -> 106.8; template-update 62.4 -> 61.5; list-diff 1130.9 -> 1117.5; ui-bundle-startup 139.0 -> 144.0 (1.04, startup is compile-bound and within its usual spread). No Gable benchmark slower by more than 5%.

Verification: cargo test --workspace green (compiler 13 incl. the unused-postfix-update test, serializer 12, VM dispatch/debugger 19/bytecode/gc/ic, new operands.rs 10, instr_count 3, cdp_client 2); test262 --engine agnijs 42225/42225, --precompile 42225/42225, --only-intl402 981/981, RegExp suites 2505/2505 under backtracking-only and linear-when-eligible, GC-stress slice (AGNIJS_GC_STRESS=16, class/assignment/for-of) 5949/5949; Gable suites under the AgniJS engine 758 passed, 2 failed (the two known V8-calibrated timing budgets). Not done: the full GC stress suite (about an hour) and clippy; rustfmt is not enforced in this repository (619 pre-existing diffs on main).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added register/immediate operand forms (BinRR/BinRI/BinAR/BinAI) and fused compare-and-branch instructions (JumpIf[Not]Cmp{RR,RI,RA,AI}) to the bytecode (agnijs-bc-5, ADR 0010 addendum, Instr still 12 bytes, J3-atomic), taught the compiler to use register-held bindings and small integer literals in place when the sibling operand cannot write locals, to branch directly on if/while/for/do-while/?: conditions (including !, &&, ||), and to compile unused postfix updates like prefix ones; the VM gets generic semantics in step and inlined Number fast paths in run. Instructions dispatched: loop-arith 23 -> 9 per iteration, calls-recursive 20 -> 13 per call, prop-monomorphic 35 -> 19 (feature instr-count, pinned in CI). Timings: loop-arith 155 -> 106 ms, loop-array-index 220 -> 196, prop-monomorphic 173 -> 149, calls-recursive 85 -> 83; Gable signal-churn 615 -> 555 ms, rxjs-operators flat; nothing regressed beyond 5%. test262 42225/42225 direct and --precompile, intl402 981/981, RegExp suites 2505/2505 under both matchers, GC-stress slice clean, Gable suites 758 passed with the two known timing failures. Risk: may_write_locals is a conservative syntactic check; any expression kind it does not know falls back to the spill path. Follow-up candidates (not created): a peephole over remaining Star/Ldar pairs around calls, and immediate forms for property keys.
<!-- SECTION:FINAL_SUMMARY:END -->
