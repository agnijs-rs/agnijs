---
id: TASK-101.3.1
title: JIT deoptimization and GC maps (reserved for a Fable agent)
status: Done
assignee: []
created_date: '2026-11-10T08:41:00Z'
updated_date: '2026-11-11T06:20:00Z'
labels:
  - needs-triage
dependencies: []
references:
  - docs/design/jit-deopt-and-gc-maps.md
  - docs/adr/0020-baseline-jit-on-cranelift-codegen-with-no-speculation.md
parent_task_id: TASK-101.3
ordinal: 285000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The baseline JIT (ADR 0020) never speculates and never holds a JS value outside the frame across a call out, so it needs neither side exits nor GC maps. Any optimisation that keeps state in machine registers, drops a generic fallback, or calls directly between compiled functions needs both first. The interfaces are stubbed in crates/agnijs-js-jit/src/handoff.rs and specified in docs/design/jit-deopt-and-gc-maps.md (frame layout, side-exit state, safe points, root finding without stack scanning, the birth-serial bound). Per the project's model assignment this is Fable work.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 The four ignored contract tests in agnijs-js-jit/src/handoff.rs pass un-ignored
- [x] #2 jit_differential passes with an optimisation that needs deopt and GC maps enabled, including under GC stress
- [x] #3 test262 passes at the baselines with AGNIJS_JIT=force
- [x] #4 No unsafe outside agnijs-js-jit's exec-style module and no native stack scanning
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. JIT crate: safe points and side exits as one table of points (pc, register locations); a frame handle per live compiled frame on a runtime-kept list (no stack scanning); deopt trampoline; the frame materialised from Rust on any exit. Codegen: JS registers cached in Cranelift variables across call outs, dirty ones spilled to the compiled frame's spill area at every safe point and listed; registers speculated as numbers (unboxed, entry and store guards are side exits). Contract tests in handoff.rs against a mock runtime with a mock heap and birth-serial bound, plus Miri-runnable tests of the handle's reads and fault-injection tests.
2. VM: Vm::jit_frames list traced by the collector; Deoptimizer: materialise writes Frame and regs, Guard demotes the register in Tier and recompiles (bounded); debugger attach materialises every live compiled frame; speculation chosen from the live frame's registers. VM tests (jit_deopt.rs): differential under GC stress, guard deopt, nested native callback, debugger.
3. Gates: test262 default, --precompile, AGNIJS_JIT=force; intl402; GC-stress slice with jit; bench geomean with the feature off within 1%; ADR 0020 addendum and design doc update; task notes.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Step 1 pushed (1b17214 on main): JIT crate mechanism (points table, FrameHandle, deopt trampoline, call-out materialisation, register caching with liveness-guarded number speculation) and the VM's frame list, GC root walk, attach walk and materialize. The four contract tests pass un-ignored; fault and panic injection added; jit_differential green under GC stress. Remaining: VM-side speculation (choose numbers from the live frame, demote on Guard, recompile; Tier code slot must become replaceable), jit_deopt.rs VM tests, gates (test262 x3, intl402, GC-stress slice, bench off-feature), ADR 0020 addendum and design-doc update.

Step 2 pushed (ee94009 on main, rebased onto the TASK-101.2 commits): VM-side speculation (Vm::jit_guess from number_candidates and the live frame), guard deopt demotes the register in Tier::demoted and drops Tier::code (now a RefCell slot; runs hold their own Rc), recompilation at the next offer, MAX_DEOPTS=4 then no guessing; JitStats::speculated; analysis.rs holds the register analysis; tests/jit_deopt.rs (6 tests, green with and without AGNIJS_GC_STRESS=1); full VM suite green with AGNIJS_JIT=force; bench jit_workloads green; ADR 0020 addendum and design doc section 8 written. Not yet run (the machine's disk filled during the gates: 0 MB free twice; only my own target dir was cleaned): test262 default/--precompile/AGNIJS_JIT=force, intl402, GC-stress test262 slice, bench geomean with the feature off, Miri on exec::tests. ACs 1 and 2 are evidenced; 3 is not yet; 4 holds by construction (unsafe only in exec.rs, frames found via JitState::frames).

test262 gates run locally after the push (release, --features jit, checkout at the PIN e4fbf57): AGNIJS_JIT=force 42231/42231 (882s), default 42231/42231, --precompile 42231/42231, --only-intl402 981/981, AGNIJS_GC_STRESS=16 language slice 5949/5949; 0 regressions, 0 newly passing in every run. AC 3 checked.

Remaining gates, run after the push. Bench geomean with the feature off: bench built at HEAD and at the parent 7ed8c27 (release, no jit feature, so no tier state either), agnijs engine, 26 workloads, three interleaved runs each after a discarded warm-up; geomean new/old per pair 0.966, 1.004, 1.000, pooled over the per-workload medians 0.9996 (within 1%). A control pair of the same binary against itself gave 1.024, so this desktop's noise floor is about 2 to 3%; the largest single moves (string-concat 1.056, bundle-snapshot 0.960) are inside it. Miri on agnijs-js-jit exec::tests (nightly, 2026-11-07): the first run found UB in the test fixture itself, a Val written into a [u8; 32] leaves padding uninitialised and a byte array may not hold uninitialised bytes; the fixture is now a struct with typed fields at the same offsets (072a5bb). Three tests green under Miri, and the crate's stable tests and clippy -D warnings are green. The crate is not rustfmt-clean, like the rest of the workspace (no rustfmt config or CI step), so it was left as written.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Deoptimisation and GC maps for the baseline JIT, in two commits on main (1b17214, ee94009) plus a Miri fixture fix (072a5bb). The JIT crate keeps a points table (safe points and side exits, each with the pc and where every live register is), a FrameHandle per live compiled frame on a runtime-kept list, a deopt trampoline and call-out materialisation; registers are cached in Cranelift variables and spilled at safe points; registers the VM guesses are numbers run unboxed behind entry and store guards. The VM guesses from the analysis's number candidates and the live frame, demotes a register when its guard fails, recompiles the block at the next offer and stops guessing after four deopts. Verification: the four formerly ignored handoff contract tests pass; jit_differential, jit_inline_cache, jit_deopt (6 new tests), snapshot and gc pass with AGNIJS_JIT=force and under AGNIJS_GC_STRESS=1; test262 at the PIN with the JIT forced, default, precompiled, intl402 and the GC-stress slice all pass with 0 regressions; the bench geomean with the feature off is 0.9996 of the parent commit; Miri is green on exec::tests; unsafe lives only in exec.rs and frames are found via the runtime list, never by stack scanning. Risks and follow-ups: the 1% bench gate is below this machine's 2 to 3% noise floor, so a CI-side check needs more runs or a looser threshold; MAX_DEOPTS=4 and the number-only speculation are first settings, ADR 0020's addendum and design doc section 8 record them.
<!-- SECTION:FINAL_SUMMARY:END -->
