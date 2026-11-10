---
id: TASK-101.3
title: Baseline JIT behind a feature flag
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-09T01:52:00Z'
updated_date: '2026-11-10T09:19:00Z'
labels:
  - needs-triage
dependencies:
  - TASK-101.1
  - TASK-101.2
  - TASK-100.12
parent_task_id: TASK-101
ordinal: 231000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
An optional baseline (template) JIT behind a cargo feature or separate crate: compile hot bytecode to machine code without optimization, using inline-cache feedback, with deoptimization back to the interpreter so breakpoints, stepping, hot update and the memory and interrupt limits keep working. Choose the code generator (Cranelift or a hand-rolled assembler) in the ADR from 101.1 before starting. Unsafe confined to one small crate. The interpreter stays the default and the portable fallback.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Hot functions run as machine code on the supported architectures with a measured speedup on gable-js-bench workloads, and the interpreter fallback is selected on every unsupported target
- [ ] #2 test262 passes at the baselines in a JIT-forced mode and a differential fuzz test against the interpreter runs in CI; debugger and interrupt tests pass with the JIT enabled
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
Groundwork split (maintainer, 2026-11-10): Opus builds everything that does not need Fable; deoptimization and GC maps / frame-state reconstruction are specified and stubbed for a later Fable agent.
1. Backend ADR (0020): verify Cranelift against licences, pure Rust, unsafe confinement, binary size, build time and the wasm32/iOS story; record the decision.
2. J5 value half: give Value a defined layout (repr(C, u8)) with layout tests; bench the interpreter.
3. New crate agnijs-js-jit (workspace member, only engine crate with unsafe, confined to one exec-memory module plus the entry trampoline); feature jit on agnijs-js-vm, off by default; runtime probe with interpreter fallback; never compiled for wasm32/iOS.
4. Baseline codegen with Cranelift for a subset (register/constant loads and stores, number arithmetic and comparisons in all operand forms, fused compare-branch, jumps); every other supported instruction calls the instruction's generic semantics (Vm::step) through one helper, running any frame it pushes to completion; per-function eligibility so nothing needs a side exit; hotness counters at entry and back-edges, OSR entry at loop headers.
5. Differential harness: interpreter vs JIT on the same functions; bounded test262 subset and the micro benches in a JIT-forced mode.
6. Hand-off spec for deopt and GC maps (ADR addendum + stub traits + ignored tests).
7. Gates: test262 default/precompile/intl402/RegExp x3 once at the end with the feature off and on, targeted gc-stress subset, interpreter bench within 5% with the feature off, Gable suites via the path-patched harness.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
From TASK-101.1 (ADR 0019): code generator decided as Cranelift (one IR for x86-64 and AArch64, verifier, cranelift-object for AOT in TASK-101.5); measure compile time per function first and reopen the choice if it exceeds about a fifth of the gain on the Gable set. The baseline keeps the interpreter frame layout (Sparkplug-style), so deopt is continue-at-pc and OSR a jump. Prerequisites: defined Value and object-header layout (J5), one JS entry point (J8, TASK-100.19), attention word (J7, TASK-100.17). Cargo feature jit on agnijs-js-vm, crate agnijs-js-jit (only engine crate with unsafe), runtime executable-memory probe with interpreter fallback; never compiled for iOS or wasm32.

Groundwork landed (Opus, 2026-11-10): commits c122932 (the defined Value layout) and f9ad2a1 (the baseline JIT), plus a follow-up that gates the tiering state behind agnijs-js-compiler/tier.

Backend: ADR 0020. Cranelift 0.137.1 is used for code generation only (codegen, frontend and native, with default features off). It does not use cranelift-jit; executable memory is managed by agnijs-js-jit's own exec module.
- The license check passes (168 packages).
- The feature adds about 5.5 MB to the binary and about 100 s to a clean build.
- Code generation is enabled only for x86-64 and AArch64 on Windows, Linux, macOS and Android (cfg jit_host). Other targets fall back to the interpreter. The wasm32 build was not checked, because the target is not installed.
- macOS does not use MAP_JIT yet, so under the hardened runtime it falls back to the interpreter.

Design: the tier never speculates. It keeps the interpreter's frame (ADR 0019 J1) and never holds a JS value outside the frame across a call out. Calls exit compiled code and re-enter it through Vm::jit_run. As a result it needs no deoptimization and no GC maps, and the birth-serial bound is unchanged.
- Inline: constants, moves, number arithmetic, bitwise operators, comparisons, fused compare-and-branch, truthiness and tag branches, Inc/Dec, ToNumeric, CheckTdz, jumps and polls.
- Everything else calls Runtime::slow, which first tries the interpreter's IC, global, env and string fast paths.
- Tiering: AGNIJS_JIT=off|on|force (or Vm::set_jit_mode), with hotness counters in Feedback::tier, threshold 1000.
- Eligibility is checked per block before code generation. In mode on, a profitability rule (at most one call out per six instructions) leaves blocks that would mostly call out to the interpreter.

Measured (bench, median of 5, interpreter to on / force):
- loop-arith: 104.9 to 35.3 / 36.2 ms (about 2.9x faster).
- Property and call workloads: within noise in mode on, and 1.16 to 1.40x slower when forced, because of the cost of each call out.
- Compile time: 0.4 to 0.6 ms per block.

Tests:
- crates/agnijs-js-vm/tests/jit_differential.rs: 14 tests comparing the interpreter with force and on, covering edge values, mixed types, ICs, calls, exceptions, globals, interrupts, GC stress and profitability.
- crates/agnijs-js-bench/tests/jit_workloads.rs: every workload's checksum agrees in all three modes.
- crates/agnijs-js-jit/tests/enter.rs.
- The VM test suite passes with AGNIJS_JIT=force.

Gates:
- test262: default and --precompile 42231/42231; intl402 981/981; RegExp CI set 2511/2511 under auto, backtracking-only and linear-when-eligible. All of these hold with the feature off and with AGNIJS_JIT=force; the default set also passes with AGNIJS_JIT=on.
- GC stress 16 subset (literals/regexp, RegExp/prototype/exec, statements/class): 4722/4722 with the feature off and forced, at --timeout-ms 120000. At the default 10 s limit, 4 literal-regexp tests time out in both modes.
- Gable suites (path-patched native/engine-harness, restored afterwards): 759 passed, 1 failed, with the feature off and with the feature on and AGNIJS_JIT=force (65.7 s against 43.4 s). The failure is the known yjs per-change timing test.
- Bench with the feature off: no change against the pre-JIT build from the same path. Builds from different checkout paths differ by up to 10% on the call workloads, through code layout alone.

Hand-off: docs/design/jit-deopt-and-gc-maps.md, with the stubs and 4 ignored contract tests in agnijs-js-jit/src/handoff.rs. Follow-ups are TASK-101.3.1 (deopt and GC maps, Fable) and TASK-101.3.2 (inline IC hit paths, MAP_JIT).

ACs are left unchecked:
- AC1: there is a measured speed-up only on numeric blocks; gable-js-bench workloads have not shown a win yet.
- AC2: the differential tests do not run in CI yet (CI does not build --features jit), and there is no fuzzer.
<!-- SECTION:NOTES:END -->
