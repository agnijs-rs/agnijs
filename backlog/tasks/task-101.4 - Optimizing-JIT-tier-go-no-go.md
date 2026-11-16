---
id: TASK-101.4
title: Optimizing JIT tier (go/no-go)
status: Done
assignee:
  - '@claude'
created_date: '2026-11-09T01:52:00Z'
updated_date: '2026-11-16T14:34:00Z'
labels:
  - needs-triage
dependencies:
  - TASK-101.3
documentation:
  - docs/adr/0028-no-optimizing-jit-tier-the-baseline-stays-the-top-tier.md
parent_task_id: TASK-101
ordinal: 232000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Only if the baseline JIT's results justify it: an optimizing tier with type specialization from inline-cache feedback, inlining, and guarded deoptimization. Decide go or no-go from the 101.3 measurements and record it.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A go or no-go decision is recorded with the evidence; if go, the optimizing tier shows a measured gain on numeric and property-heavy workloads without any test262 regression or debugger regression
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Evidence from 101.3: same-binary interleaved rounds, JIT off/on/force, on agnijs-js-bench and Gable's benches.
2. Where Gable's time goes: winprof profile of the interpreter on the Gable benches, plus AGNIJS_JIT_STATS counters (call outs, direct calls, abandoned blocks).
3. Estimate what an optimizing tier (type specialization from IC feedback, inlining, guarded deopt) would buy on those workloads, its cost and soundness risk (jit-review-2026-11), and the alternatives.
4. Record the decision as an ADR (docs/adr/0028) and link it from ADR 0020 and the findings doc.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Decision: no-go, recorded as ADR 0028 (docs/adr/0028-no-optimizing-jit-tier-the-baseline-stays-the-top-tier.md). Evidence is in docs/js-engine-performance-findings.md, section 'Performance-tier close-out, 2026-11-16', measured at 0f03d8f:
- Baseline tier, mode on over the interpreter (one binary, 9 interleaved rounds): numeric, element and property loops 0.23 to 0.33, calls-recursive 0.36, closures 0.79, everything else within 3%; micro geomean 0.78. Gable's nine benches: geomean 0.98, one at 0.72, six at 1.015 to 1.036. Forced: Gable rxjs and signals 0.72 to 1.03 (geomean 0.89), templates and list-diff 1.09 to 1.20.
- Where Gable's time goes in the interpreter (winprof, 8 benches): dispatch 21 to 48%, out-of-line property access 15 to 44%, calls 8 to 29%, collector 0 to 13%, system allocator under 3%, regex under 1.5%.
- Forced profile: the two benches that win spend 60% of their time in machine code; the three that stay level 33 to 40%, with 1.5 to 2.4 call outs per direct call against 0.2 to 0.3.
- Estimate for an optimizing tier on Gable's rxjs and signal benches: 1.2 to 1.5 times over the forced baseline if it halves the time in machine code, less on templates, nothing at start-up or on iOS. Cost: a tier larger than the baseline's 9,000 lines, with many kinds of speculation where the baseline's one kind already produced TASK-103.34.
- Alternatives that pay more, in order: megamorphic property cache, the call path (call/apply, getters, new), denser bytecode, the 1.46x gap to QuickJS-NG's interpreter, start-up and memory.
No code changed; no follow-up tasks were created (the ADR lists directions, to become tasks when scheduled).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
No-go on an optimizing JIT tier, recorded with its evidence as ADR 0028 and reopenable on the conditions listed there. The baseline tier gains 3 to 4 times on numeric and property loops but 2% on Gable's workloads as a whole, whose time is in calls, out-of-line property access and the runtime (profiles and counters in the performance findings, 'Performance-tier close-out, 2026-11-16'). An optimizing tier would plausibly add 1.2 to 1.5 times on Gable's rxjs and signal benches for hot code only, at a large cost in code, soundness risk and maintenance, and nothing on iOS. Performance work goes to the interpreter and runtime instead: megamorphic property access, the call path, bytecode density, start-up and memory.
<!-- SECTION:FINAL_SUMMARY:END -->
