---
id: TASK-101.5
title: Native AOT compilation research
status: Done
assignee:
  - '@claude'
created_date: '2026-11-09T01:52:00Z'
updated_date: '2026-11-16T14:36:00Z'
labels:
  - needs-triage
dependencies:
  - TASK-101.1
  - TASK-100.12
documentation:
  - docs/aot-research.md
parent_task_id: TASK-101
ordinal: 233000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Research track: ahead-of-time compilation of JavaScript to native code at build time for a typed or restricted subset, with fallback to the interpreter for dynamic code. The only route to speed where a JIT is forbidden (iOS). Study static Hermes and Porffor; produce a feasibility report and a prototype scope, not a product.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A research note in docs/ compares the options with prior art, estimates cost and payoff, and recommends go, no-go or defer
- [ ] #2 If go, a prototype scope and follow-up tasks are written
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Prior art from primary sources: static Hermes, Porffor, weval/SpiderMonkey, Hopc, Bun --aot, V8 jitless, platform policy (Apple, Android, WASM), cranelift-object.
2. Options for AgniJS: baseline AOT of bytecode through the existing Cranelift code generator (ADR 0019/0020), typed-subset AOT, and no AOT (interpreter + snapshots).
3. Cost and payoff estimates from our own baseline JIT measurements (forced mode is the upper bound of a non-speculating AOT).
4. Research note docs/aot-research.md with a recommendation; if go, prototype scope and follow-up tasks.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Model assignment (maintainer, 2026-11-08): Opus.

From TASK-101.1 (ADR 0019): AOT should reuse the baseline JIT's code generator (Cranelift via cranelift-object) and its extern C runtime-helper contract (J10), produce a separate artefact with its own format tag bound to the engine build, and resume in the interpreter (same frame layout, J1) for anything it does not compile.

Research note: docs/aot-research.md. Recommendation: defer.
- Prior art read from primary sources (static Hermes, Porffor, weval with SpiderMonkey, Hopc, Bun --aot, Static TypeScript; qjsc, Javy and deno compile as non-AOT contrasts; V8 jitless; Apple, Android and WebAssembly rules; cranelift-object and Wasmtime's tiers). The note lists each source and what could not be verified.
- Finding: untyped JavaScript compiled ahead of time without speculation reaches baseline-JIT speed at best (weval 2.2 to 2.8 times an interpreter on Octane; static Hermes 6 to 46% on real untyped programs and slower than its interpreter on an untyped micro-benchmark); the large gains need a sound typed dialect.
- Own measurements at 0f03d8f (forced baseline JIT as the upper bound of a baseline AOT tier): numeric and property loops 0.24 to 0.33 of interpreted time, Gable's eight benches geomean 0.98 (0.72 to 1.20). Machine code is 26 times the bytecode container on a script whose functions all run (17.4 MB against 0.68 MB).
- Other costs: no over-the-air delivery of native code on iOS, Cranelift's iOS target untested upstream, no iOS CI here, layouts must become build constants, an artefact format and loader.
- Options B (typed dialect) and C (no interpreter) are no-go; D (weval) does not apply to native iOS.
- Reopen conditions and a prototype scope are in the note. AC #2 does not apply (not a go), so no follow-up tasks were created. Model note: this task's notes assigned it to Opus; it was done in this session by the agent closing out the track.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
docs/aot-research.md compares ahead-of-time compilation options with prior art from primary sources, estimates payoff from the forced baseline JIT (the upper bound of a baseline AOT tier: 3 to 4 times on numeric and property loops, nothing on average on Gable's benches) and cost (machine code 26 times the bytecode as the code generator stands, no over-the-air updates on iOS, an untested target, a new artefact and build step), and recommends defer, with the conditions that would reopen it and a prototype scope. Criterion 1 is met; criterion 2 applies only to a go, which this is not, so it is left unchecked and no tasks were created.
<!-- SECTION:FINAL_SUMMARY:END -->
