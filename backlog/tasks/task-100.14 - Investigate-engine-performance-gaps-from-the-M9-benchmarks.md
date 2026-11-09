---
id: TASK-100.14
title: Investigate engine performance gaps from the M9 benchmarks
status: Done
assignee:
  - '@claude'
created_date: '2026-11-09T00:18:00Z'
updated_date: '2026-11-09T03:19:00Z'
labels:
  - ready-for-agent
dependencies:
  - TASK-100.10
references:
  - docs/js-engine-spec.md
documentation:
  - docs/js-engine-performance-findings.md
parent_task_id: TASK-100
ordinal: 226000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
After M9 records the Gable benchmark set and the interim-engine baseline (TASK-100.10, with the early micro-benchmarks from TASK-100.13), find out WHY the owned engine is slower or heavier than the interim engine where it is. Investigation only: profile and explain, do not tune. The fixes are a separate job: each distinct cause becomes its own task, to be implemented by a Fable agent (an investigator Opus agent hands over ranked findings; it does not fix them). Not covered by M9, whose scope is the benchmark gate and switch-over report.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Every benchmark in the M9 set and the TASK-100.13 micro-benchmarks where the owned engine is outside the proposed tolerance, or clearly slower or heavier than the interim engine, is profiled (CPU samples, allocation and GC counts, inline-cache hit rates, bytecode and dispatch statistics as appropriate)
- [x] #2 A findings document in docs/ ranks the causes by measured impact, with the evidence for each (profiles, counters, minimal reproductions) and an estimated effort and risk per fix
- [x] #3 Each distinct cause that is worth fixing is created as its own backlog task ready for a Fable agent, with a reproduction and a benchmark to prove the fix; no maintainer approval is needed to create them, only micro-optimizations that barely move the needle are left out (listed in the findings document as skipped)
- [x] #4 No engine behaviour or test262 baseline changes in this task
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Re-run the micro and Gable benchmark sets on the integration tip. 2. Sample-profile every gap workload (custom out-of-process Windows sampler, inline frames and lines) and collect counters from a temporary, uncommitted instrumentation build (opcode histogram, IC hit/miss by cause, GC count and time, deferred GC checks, frames, native re-entries, global loads). 3. Minimal reproductions for the six known M9 items. 4. Rank causes in docs/js-engine-performance-findings.md with effort and risk. 5. One Fable ticket per cause with a reproduction and benchmark. 6. Revert instrumentation; the commit is docs and backlog only.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Maintainer direction (2026-11-08): adoption of the engine is not in doubt and performance gaps will be fixed over time, so the investigator creates the Fable fix tickets itself using judgement; skip only micro-optimizations that barely move the needle.

Profiled every micro-benchmark and Gable benchmark with a gap (all except map-set, where the Gable engine is 4.5x faster) on 3ade2ef: CPU samples with a new no-admin Windows sampler (native/tools/winprof, standalone, not built by CI), plus a temporary uncommitted counter build (opcode histogram, IC hits and misses by cause, GC count and time, deferred GC checks, frames, native re-entries, global loads, string-constant allocations). Minimal reproductions run through gable-harness run on both engines. Fat LTO with one codegen unit was measured and ruled out as a cause (within noise except compile-source, about 15%).

Headline: the memory gap is the collector being deferred for all embedded execution (module bodies, host calls, jobs and native callbacks all run under native_depth > 0); the CPU gap is dispatch overhead times instruction count (Star/Ldar 50-60% of executed instructions), plus call overhead (per-call args Vec, nested run loops through Function.prototype.call), IC coverage gaps and allocation weight. The six known M9 items map to TASK-100.16 (GC mid-call), TASK-100.20 (IC), TASK-100.23 (GC laziness was the accounting; growth not counted), TASK-100.28 (lazy debug info, 3.4% of bytecode-load), TASK-100.30 (anchored regex).

Instrumentation was reverted; the diff is the findings doc, the profiler tool and backlog tasks only.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Investigation of the M9 performance gaps, findings in docs/js-engine-performance-findings.md, ranked by measured impact with evidence, effort and risk.

- Memory: the collector never runs inside a module body, host call, job or native callback, because all of them run under a native frame (`maybe_collect` returns when `native_depth != 0`). That is the whole peak-RSS gap (rxjs-higher-order 124 vs 6.7 MiB; a two-line repro 901 vs 5.4 MiB). Separately, element, slot and dictionary growth is not counted towards collection or the memory limit (array-methods: 0 GCs, 82 MB heap).
- CPU: dispatch overhead (out-of-line `step` with a 1.2 KiB frame, pc and acc in memory, four flag tests per instruction; 26-55% self time on the Gable set) multiplied by instruction count (Star/Ldar 50-60%), then call overhead (per-call args Vec, nested run loops; `Function.prototype.call` is under 94-100% of Gable samples), IC coverage gaps (arrays, polymorphic, accessors; name re-interning on every miss), allocation weight and per-collection fixed cost.
- Smaller: global lookups, promise internals, front-end allocation, quadratic shape transitions at realm creation, bytecode decode growth and debug info (3.4%), built-in allocation, and the linear anchored-regex failure (4.5 ns per char).

Created 15 Fable tickets, TASK-100.16 to TASK-100.30, each with a reproduction and a benchmark. Skipped micro-optimizations and the LTO measurement are listed in the doc. Added native/tools/winprof (standalone sampling profiler plus analyze.mjs). No engine code or test262 baseline changed.
<!-- SECTION:FINAL_SUMMARY:END -->
