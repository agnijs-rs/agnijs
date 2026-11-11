---
id: TASK-101.3.1
title: JIT deoptimization and GC maps (reserved for a Fable agent)
status: In Progress
assignee: []
created_date: '2026-11-10T08:41:00Z'
updated_date: '2026-11-11T06:13:00Z'
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
<!-- SECTION:NOTES:END -->
