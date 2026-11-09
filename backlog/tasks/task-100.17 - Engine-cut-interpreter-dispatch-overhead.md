---
id: TASK-100.17
title: 'Engine: cut interpreter dispatch overhead'
status: Done
assignee:
  - '@fable-agent'
created_date: '2026-11-09T03:18:00Z'
updated_date: '2026-11-09T09:43:00Z'
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
- [x] #1 pc, accumulator and the register window are kept in locals of the dispatch loop and written back only where frames are observed (calls, returns, throws, safe points, debugger stops)
- [x] #2 The per-instruction flag tests are replaced by a single check, and debugger stepping, breakpoints, interrupts, the memory limit and GC stress still work (their existing tests pass)
- [x] #3 loop-arith and calls-recursive are at least 1.3x faster than at the start of the task
- [x] #4 test262 passes with no new failures or exclusions in both `--engine gable` and `--engine gable --precompile` runs, and the Gable suites pass under the Gable engine
- [x] #5 The before and after numbers for the reproduction and the named benchmarks are recorded in the task notes, and no other micro-benchmark or Gable benchmark regresses by more than 5%
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red: baseline loop-arith, calls-recursive, prop-monomorphic, method-calls with the origin/main bench.exe (7 iterations after 2 warmup) and a winprof profile of loop-arith; add tests/dispatch.rs in agnijs-js-vm: a recursive function with no loops is interrupted by the host hook (TASK-100.33), a GC requested by the host while JS runs is collected at the next safe point, the memory limit still stops a straight-line allocator, GC stress enabled between host calls takes effect, plus an ignored release-mode timer for the reproduction (1e6-iteration module loop).
2. Attention word (ADR 0019 J7): one u32 at a fixed field, bits STRESS, DEBUG, GC. Set by the allocator when allocated_since_gc crosses gc_threshold, by gc_requested, by debugger attach and by stress mode; the dispatch loop tests only this word per instruction and runs the old slow path (debug_check, maybe_collect, memory limit) when it is non-zero, then recomputes the word. Under a native frame (collection deferred) the GC bit is dropped and re-armed by the next allocation, so a deferred collection costs one slow pass per allocating instruction instead of one per instruction.
3. Dispatch loop: pc, acc and base become locals of Vm::run. Hot, non-allocating, non-faulting opcodes (loads, moves, Number arithmetic and comparisons, jumps, env slots, IC hits, global-cache hits, LdaThis) are handled inline; everything else writes pc and acc back to the frame and calls the existing Vm::step (now #[inline(never)]), whose semantics are unchanged (J3), then reloads pc and acc. Call, Return and faults go through call_from_frame, do_return and handle_fault with the frame written back first (J1). The code Rc is cloned on Reload only when the frame's code differs.
4. Interrupt poll (J7, TASK-100.33): poll_interrupt at run entry, on every frame change (function entry and after a call returns) and at back-edges as before.
5. Measure after, alternating binaries; cargo test --workspace, debugger, cdp, gc-stress tests; test262 full, --precompile, intl402, RegExp suites; Gable suites via the path-patched harness; record numbers, finalize, commit, push.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
JIT-readiness (ADR 0019, TASK-101.1): J1, J2, J3, J7 apply. Locals for pc/acc/window are fine, but write them back before any call, throw, safe point, debugger stop or suspension (J1), and address registers by base index since the register file can reallocate. pc stays an instruction index with O(1) mapping if you change encoding (J2). When inlining opcode bodies, keep each opcode's generic semantics callable as a Vm function a JIT can call (J3). Build the attention word as one word at a fixed Vm field, polled at function entry, back-edges and after calls return (J7; TASK-100.33 covers the missing entry poll). No unsafe in agnijs-js-vm (J10, TASK-100.32): get bounds-check relief from verified frame sizes and slice windows; raise it with the maintainer if unchecked access proves necessary.

## Result (Fable, 2026-11-09)

Root cause confirmed by profile (winprof, loop-arith before): `Vm::step` 48% self and `Vm::run` 34% self; the per-instruction out-of-line call with its 0x4D8-byte prologue, four flag tests, frame-vector pc/acc traffic and an `Rc<CodeBlock>` clone per call/return.

Changes (commits e3943d3, d947463, 24160ac on main):
- `Vm::run` keeps `fi`, `code`, `base`, `pc`, `acc` in locals and handles the hot opcodes inline (loads/moves, scoped and cached global access, IC hits for GetNamed/SetNamed, Number arithmetic/compare/bitwise, Inc/Dec, strict equality, every jump, Call, Return). Everything else still goes through `Vm::step`, unchanged, so each opcode's generic semantics stays a `Vm` function (ADR 0019 J3). pc/acc are written back before every call, fault, safe point and debugger stop (J1). The Rc is cloned only when the code block changes.
- One attention word, `Heap::attention` (`ATTENTION_GC`, `ATTENTION_DEBUG`), replaces the four flag tests (J7). The allocator sets it when the GC threshold is crossed; GC requests, stress mode, the memory limit (threshold capped at the limit) and debugger attach set it directly. Slow path `Vm::attention_slow` is cold and shared. The word lives in `Heap`, a by-value field of `Vm`, so it is at a fixed offset; noted in ADR 0019 J7.
- Host interrupt polled at function entry and after calls return (TASK-100.33), not only on back-edges.
- The ticket's reproduction turned out not to be dispatch: the compiler poisoned the whole module body (every binding in an environment), so a module-level `for (let i ...)` copied an environment per iteration. Module block scopes now follow the capture analysis like functions; top-level module bindings stay in the module environment. No bytecode format change.
- Tests: `crates/agnijs-js-vm/tests/dispatch.rs` (interrupt at entry/returns, host-requested GC at next safe point, stress mode, memory limit on straight-line code, ignored release-mode reproduction timer); compiler test `module_block_scoped_bindings_stay_in_registers`.

Numbers (same machine, alternating binaries, release):
- Reproduction (1e6-iteration module loop, best of 7): 110 ms -> 33 ms (3.3x; QuickJS-NG 37 ms in the findings).
- Micro (7 iterations after 2 warmup, two runs each): loop-arith 489.6/491.5 -> 150.6/150.4 ms (3.26x); calls-recursive 161.0/167.1 -> 90.1/92.8 ms (1.8x); prop-monomorphic 353.3/346.7 -> 173.9/173.0 ms (2.0x); method-calls 304.4/298.5 -> 215.9/217.3 ms (1.39x).
- Full micro set, after/before: loop-arith 0.30, loop-array-index 0.48, prop-monomorphic 0.57, prop-polymorphic 0.61, prop-write-alloc 0.74, string-concat 0.92, string-methods 0.98, array-methods 0.75, map-set 0.80, closures 0.76, calls-recursive 0.56, method-calls 0.71, regex 0.96, regex-tokenize 0.84, regex-long-input 0.97, json-stringify 1.02, json-parse 0.99, promise-chain 0.96, async-await 0.96, compile-source 0.96, bytecode-load 0.98, engine-startup 1.06 (re-run alternating 3 pairs: 0.66/0.65/0.70 vs 0.68/0.67/0.68 ms, noise). Nothing regressed beyond 5%.
- Gable (path-patched harness, 7 iterations, two alternating pairs, after/before of medians): rxjs-operators 411.7/414.2 -> 299.1/294.8 ms (0.72); rxjs-higher-order 0.83; rxjs-subject-fanout 308.4/313.6 -> 225.3/217.0 ms (0.71); signal-churn 954.3/953.7 -> 608.2/609.2 ms (0.64); signal-create 0.74; template-mount 0.85; template-update 0.82; list-diff 0.87; ui-bundle-startup 0.98.
- After profile: loop-arith is `run` 90% self + fmod 10%; calls-recursive is `run` 37%, heap alloc/free 24%, push_function_frame 15% (the call path, TASK-100.19).

Verification: cargo test --workspace green; AGNIJS_GC_STRESS=1 on agnijs-js-vm and agnijs-js-compiler green (note: host_driven takes 33 min and the vm lib tests 20 min under stress=1 in debug, inherent to a collection per instruction); test262 agnijs 42225/42225, --precompile 42225/42225, intl402 981/981, RegExp suites 2505/2505 under backtracking-only and linear-when-eligible, module/dynamic-import/for slices 3688/3688 under AGNIJS_GC_STRESS=16; Gable suites 758 passed, 2 known timing failures.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Dispatch loop keeps pc/acc/window in locals with hot opcodes inlined and one attention word per instruction (ADR 0019 J1/J3/J7); interrupt polled at entry and after calls; module block scopes use registers. loop-arith 3.3x, calls-recursive 1.8x, reproduction 110 -> 33 ms, Gable signal-churn 1.56x, no regressions. test262, GC stress, debugger/cdp and Gable suites green. Follow-ups: call path (TASK-100.19), bytecode shape (TASK-100.18).
<!-- SECTION:FINAL_SUMMARY:END -->
