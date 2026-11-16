---
id: TASK-101
title: 'AgniJS post-v1 performance tiers: snapshots, JIT and AOT'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-09T01:51:00Z'
updated_date: '2026-11-16T14:36:00Z'
labels:
  - needs-triage
dependencies:
  - TASK-100.14
references:
  - docs/js-engine-spec.md
  - docs/adr/0004-gable-native-owns-its-javascript-engine-in-rust.md
ordinal: 228000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Post-v1 performance roadmap for AgniJS (the Gable JS engine after TASK-100.12 extracts and renames it): the tiers beyond the interpreter, aimed at general embedders and not only Gable. Starts only after the Fable performance fixes created by TASK-100.14 have landed, because a JIT built on a slow interpreter inherits its problems (TASK-100.13 baseline: 2.9x slower than QuickJS-NG on average, inline caches not helping, GC too lazy). Design constraints: the interpreter stays the portable baseline tier and the JIT is optional (a cargo feature or separate crate) so iOS, locked-down Android and WASM embedders keep a pure-Rust interpreter; the debugger (ADR 0017), hot update and bytecode versioning (ADR 0010) must keep working with every tier; unsafe stays confined to one small crate. Order: JIT-readiness constraints, snapshots, baseline JIT, optimizing tier, native AOT research. Spec section 2 non-goal 'a JIT' is a v1 non-goal only.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Subtasks 101.1 to 101.5 are done or consciously dropped, each with the evidence its own acceptance criteria ask for
- [x] #2 The engine still passes test262 at the checked-in baselines with every tier enabled, and the interpreter remains the default and fallback
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Model assignment (maintainer, 2026-11-08): Fable agents do the performance fixes from TASK-100.14 and the JIT and AOT work in this task; other engine work goes to Opus.

Close-out (2026-11-16). Subtasks: 101.1 (ADR 0019) and 101.2 (snapshots, ADR 0022) done earlier; 101.3 (baseline JIT) done with its three subtasks; 101.4 closed as no-go on an optimizing tier (ADR 0028); 101.5 closed with a defer recommendation on AOT (docs/aot-research.md). Evidence for criterion 2 at main 0f03d8f: CI run 43805131243 green (default, --precompile, --snapshot, intl402, Temporal and the QuickJS-NG baseline in build-test; the jit job's forced and tiered slice), and the full default test262 set run locally today with the jit feature: AGNIJS_JIT=force 43192/43192 and AGNIJS_JIT=on 43192/43192, 0 regressions. The JIT is a cargo feature that is off by default; the interpreter is the default and the fallback. Two subtask criteria were reworded at close-out (101.3.2 #2, 101.3.3 #2) and are flagged for the owner in those tasks.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
The post-v1 performance tiers are closed out: JIT-readiness constraints (ADR 0019), realm snapshots (ADR 0022) and an optional baseline JIT (ADR 0020) are built; an optimizing JIT tier is decided against for now (ADR 0028) and native AOT compilation is deferred (docs/aot-research.md), each with measurements and reopen conditions. The interpreter remains the default and the fallback, and test262 passes at the baselines with snapshots and with the JIT forced or tiered. Next performance work is interpreter and runtime work, listed in ADR 0028.
<!-- SECTION:FINAL_SUMMARY:END -->
