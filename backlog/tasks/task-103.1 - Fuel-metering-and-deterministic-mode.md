---
id: TASK-103.1
title: Fuel metering and deterministic mode
status: Done
assignee:
  - '@claude'
created_date: '2026-11-09T02:14:00Z'
updated_date: '2026-11-10T21:56:00Z'
labels:
  - ready-for-agent
dependencies: []
parent_task_id: TASK-103
ordinal: 236000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Instruction-count (fuel) budgets that give the same result on every run, on top of the existing interrupt and memory limits (TASK-100.8), plus a deterministic mode: seeded Math.random, fixed or host-supplied Date and time zone, no ambient clocks, stable iteration where the spec leaves order open. Useful for plugin hosts, serverless, games, blockchains and reproducible tests. Model: Opus.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 An embedder sets a fuel budget per call or realm; exhaustion stops execution with a host-visible error, deterministically at the same instruction on every run and platform
- [x] #2 Deterministic mode gives byte-identical results across runs, platforms and engine builds for a documented test corpus, and every remaining source of nondeterminism is documented or host-supplied
- [x] #3 Overhead with fuel disabled is within noise on gable-js-bench; test262 baselines unchanged
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Fuel: Vm counts every dispatched bytecode instruction against a host budget when one is set (Engine::set_fuel / Engine::fuel in agnijs-engine-api, default Unsupported). The budget raises a FUEL bit in the attention word, so the fuel-off dispatch loop is unchanged; the fuel-on path charges in the attention branch. Exhaustion is an uncatchable fault (Error::OutOfFuel). Fuel forces the interpreter (like an attached debugger), so counts are the same with the jit feature.
2. C API: AGNIJS_OUT_OF_FUEL status, agnijs_set_fuel / agnijs_clear_fuel / agnijs_get_fuel, ABI minor bump, header regenerated; tick TASK-103.3 AC1.
3. Deterministic mode: per-engine Math.random state seeded by a new Host::random_seed hook (today it is a clock-seeded thread-local); a checked-in corpus with golden output run in CI on every OS; docs/determinism.md listing every remaining source of nondeterminism.
4. Verify: agnijs-js-bench before/after (fuel off), test262 default and --precompile at 42231/42231, jit feature tests.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
2026-11-10, implementation (worktree branch fuel, pushed to main):

Fuel:
- Engine::set_fuel(Option<u64>) / Engine::fuel() in agnijs-engine-api (default Unsupported; the QuickJS adapter inherits it). One unit per dispatched bytecode instruction, engine-wide: realms, host callbacks, natives' callbacks (forEach, sort, getters) and promise jobs all draw from it. Metering one call = set a budget before it, read fuel() after; metering a realm = give it its own engine.
- Exhaustion is Fault::OutOfFuel -> Error::OutOfFuel; uncatchable (no catch, no finally), and every later call fails until a new budget is set or it is cleared.
- ATTENTION_FUEL is a third attention bit. The charge lives in a new cold attention_slow_interp, not in the loop, so the fuel-off dispatch loop is the machine code it was before. An inline fast path was tried first and cost up to +6% on regex-long-input with fuel off, so it was moved out of line.
- A budget forces the interpreter, like J9: jit_run_slow and enter_callee refuse while fuel_on, and JitRt::attention exits compiled code when a host callback sets a budget mid-run. A mutation check showed the jit_run_slow guard matters: without it, Force mode counted 9013 instead of 11016.
- Fuel on costs about 1.65x on a tight loop.

C API (ABI 1.1):
- agnijs_set_fuel, agnijs_clear_fuel, agnijs_get_fuel (NOT_FOUND when no budget is set) and agnijs_set_random_seed.
- AGNIJS_OUT_OF_FUEL = 4. A host function returning it passes it on.
- The seed is a function, not an options field: versioned() refuses a struct smaller than the library's, so a trailing field would break 1.0 callers.
- Header regenerated with cbindgen. Covered by the C test, basics.rs and fault_injection.rs.
- Docs: docs/c-api.md, ADR 0021 addendum, ADR 0019 addendum, and the spec section 7, J7 and J9 rows.

Deterministic mode:
- Math.random state is per Vm, seeded lazily from the new Host::random_seed (default: clock xor a counter) through splitmix64 into xorshift64*. Engine::set_random_seed reseeds.
- Float32 and Float64 stores write the one quiet NaN. On x86, 0/0 stored 0xFFF8..., which ARM would not, so buffer bytes differed by platform.
- Corpus: crates/agnijs-js-vm/tests/determinism/*.js with golden .txt, 9 programs covering random, date/time zone, numbers, objects/order, text/regex/Unicode, Intl, job order, errors and bytes. CI runs it on all three OSes, interpreted and with AGNIJS_JIT=force (both pass locally). It also checks identical fuel per run, and that metering does not change output.
- docs/determinism.md lists the host inputs to fix and what still varies:
  - libm transcendentals
  - GC timing (WeakRef/FinalizationRegistry) and the memory limit point
  - native stack depth
  - interrupts
  - engine version (Unicode/ICU4X data, bytecode, so fuel costs)
  - host defaults that read the clock
- Audited: the compiler's foldhash maps are only merged set-into-set or looked up; the VM's hashbrown maps are lookups or sorted (Map index).

Verification:
- cargo test --workspace: all green.
- agnijs-js-vm --features jit with AGNIJS_JIT=force: green.
- agnijs-capi with fault-injection: green.
- CMake C tests and examples: 6/6.
- capi clippy as in CI: clean.
- test262 agnijs 42231/42231, both default and --precompile, 0 regressions.

Benchmarks (agnijs-js-bench, agnijs engine, interleaved before/after rounds, 10 iterations, median ratios):
- Full suite, final build vs b54b9ca: geomean 1.0040 (min-based 1.0083) over 21 workloads.
- The fuel-only build was within noise: geomean 1.0007 and 1.0017 in two runs; regex-long-input 0.977 to 0.987.
- With deterministic mode, regex-long-input is consistently +4 to 5%: 1.042 to 1.075 at --scale 10.
  - The RNG code is not the cause. An unused u64 field added to Vm reproduces it, and so does storing the state in Realm (+8, +16 or +32 bytes). Only a process-wide static avoided it, and that would break per-engine determinism.
  - It looks like a code or data layout effect of growing an engine struct, not a cost of the feature. Kept the clean design (Vm::rng); worth a look with a profiler on a machine that has one.

TDD note: the seams (Engine trait fuel and seed methods, C API functions, determinism corpus) were chosen without user confirmation; this ran as a subagent.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Fuel and deterministic mode for AgniJS.

- Fuel: Engine::set_fuel and Engine::fuel give an engine-wide instruction budget, charged in the attention slow path, so the fuel-off dispatch loop is unchanged. Exhaustion is an uncatchable Error::OutOfFuel at the same instruction on every run. A budget forces the interpreter, so the count does not depend on the tier.
- C API ABI 1.1: agnijs_set_fuel, agnijs_clear_fuel, agnijs_get_fuel, AGNIJS_OUT_OF_FUEL and agnijs_set_random_seed.
- Deterministic mode: Math.random is per engine and seeded by Host::random_seed or Engine::set_random_seed, and NaN is canonical in Float32 and Float64 stores. A golden corpus runs on every CI OS, interpreted and with the JIT forced. docs/determinism.md documents what still varies.
- Verified: workspace tests, the JIT-forced VM suite, capi with fault injection, C tests 6/6, and test262 42231/42231 both default and --precompile.
- Bench: fuel-off is within noise. The final geomean is 1.004, but regex-long-input is +4 to 5% from a layout effect of adding the RNG state to Vm (an unused field reproduces it); see the notes.
<!-- SECTION:FINAL_SUMMARY:END -->
