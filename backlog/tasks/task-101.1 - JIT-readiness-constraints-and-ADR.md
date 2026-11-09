---
id: TASK-101.1
title: JIT-readiness constraints and ADR
status: Done
assignee:
  - '@claude'
created_date: '2026-11-09T01:51:00Z'
updated_date: '2026-11-09T05:54:00Z'
labels:
  - needs-triage
dependencies:
  - TASK-100.14
parent_task_id: TASK-101
ordinal: 229000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Record the constraints that keep a later JIT possible, as an ADR plus a short section in the engine spec, and check the current engine against them. Constraints to cover: frame layout and register mapping, stable per-site inline-cache feedback (shape metadata readable by a JIT), GC roots and write barriers a JIT can emit (stack maps or conservative scanning), explicit side-effect and exception boundaries for deoptimization, and a public embedding API that does not lock the engine to interpretation. Engine crates stay Gable-agnostic (ADR 0016).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 An ADR in docs/adr records the JIT-readiness constraints and the tiering and deoptimization strategy, including the optional-feature decision and the platform story (macOS entitlement, iOS, locked-down Android, WASM)
- [x] #2 The spec has a short section listing the constraints; every existing engine design that violates one is listed with a proposed fix, created as backlog tasks where worth doing
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Read ADRs 0008-0012, 0017, the findings doc and the gated tickets (100.16-100.21, 101.2-101.5); inspect the interpreter, IC, heap and object layout against each constraint.
2. Write ADR 0019 (numbering shared with Gable, whose last is 0018): constraints for frames, bytecode, IC feedback, object/value layout, GC roots and barriers, deopt points, embedding API, unsafe confinement; tiering and deopt strategy; optional-feature decision; platform story; code-generator choice criteria for 101.3.
3. Add a short JIT-readiness section to docs/js-engine-spec.md listing the constraints and every current violation with its proposed fix; link from the ADR index.
4. Create backlog tasks only for violations worth fixing now; append a JIT-readiness note to 100.16-100.21.
5. No engine code change expected; docs only, so no test262 run needed.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Model assignment (maintainer, 2026-11-08): Opus.

Ordering (maintainer, 2026-11-08): this ADR gates TASK-100.17 and TASK-100.18 so the dispatch and bytecode rewrites avoid choices that block a JIT. It is cheap and stays ahead of the engine extraction. The rest of TASK-101 follows the extraction (TASK-100.12).

ADR 0019 (numbering shared with Gable, whose adoption ADR is 0018) records constraints J1-J11, tiering (Sparkplug-style baseline on the interpreter frame layout; optimizing tier deopts to interpreter frames at instruction boundaries), the jit cargo feature plus agnijs-js-jit crate, the platform table (macOS entitlement opt-in, never on iOS or wasm32, probe-and-fallback on Android/Windows ACG) and the code generator (Cranelift, with a compile-latency check in 101.3). Spec section 14 lists the constraints and ten current gaps with fixes. New tasks: TASK-100.32 (forbid unsafe in engine crates), TASK-100.33 (interrupt hook is polled only on backward jumps, so loop-free recursion cannot be interrupted). JIT-readiness notes appended to TASK-100.16 to 100.21 and TASK-101.2, 101.3, 101.5. Docs and backlog only; no engine code changed, so no test262 run.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added ADR 0019 (docs/adr/0019-jit-readiness-constraints-tiering-and-deoptimization.md) fixing eleven JIT-readiness constraints (frames, pc, opcode semantics and deopt atomicity, feedback tables, layout, GC rooting and barriers, attention word, call entry, debugger and hot update, embedding API and unsafe confinement, snapshot serialisability), the tiering and deoptimization strategy, the optional-feature decision, the platform story and the Cranelift choice for TASK-101.3. Spec section 14 summarises the constraints and lists every current gap with its fix and owning ticket; two new tasks cover the gaps not owned elsewhere (TASK-100.32, TASK-100.33). The gated tickets carry notes naming the constraints that bind them. Docs-only change; open questions for the maintainer: Cranelift vs hand-rolled, whether unchecked register access is ever acceptable in agnijs-js-vm, and whether to fold the defined Value layout into TASK-100.21.
<!-- SECTION:FINAL_SUMMARY:END -->
