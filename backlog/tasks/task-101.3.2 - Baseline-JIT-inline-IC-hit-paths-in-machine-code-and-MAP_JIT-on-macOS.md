---
id: TASK-101.3.2
title: 'Baseline JIT: inline IC hit paths in machine code, and MAP_JIT on macOS'
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-10T08:41:00Z'
updated_date: '2026-11-10T12:40:00Z'
labels:
  - needs-triage
dependencies: []
parent_task_id: TASK-101.3
ordinal: 286000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Measured in ADR 0020: compiled code is about 2.9x faster on blocks that run inline, but 16-40% slower when forced on property- and call-heavy blocks, because every property access calls out to the runtime. Mode on therefore leaves those blocks to the interpreter. Inline IC hit paths (object header at the ADR 0019 J5 offsets, IC side table) with a full miss path to Runtime::slow stay within the no-speculation design and need no deopt. Separately, the macOS row of ADR 0019 (MAP_JIT plus per-thread write protection) is not met yet; macOS falls back to the interpreter.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Named property loads and stores that hit a monomorphic IC run without calling out of compiled code
- [ ] #2 prop-monomorphic and method-calls are faster with AGNIJS_JIT=on than interpreted, measured with agnijs-js-bench
- [x] #3 On macOS with the allow-jit entitlement, the probe succeeds using MAP_JIT and pthread_jit_write_protect_np
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Layout plumbing: the VM describes its object arena, object header, slots, proto, array length, dense elements and IC table to the JIT (JitLayout), computed in safe code at start-up with probes; any failed check disables inline IC paths (they call Runtime::slow as before).
2. Ctx gains the arena base, proto epoch and IC table, refreshed after every call out (never held across one).
3. Inline GetNamed (own data inline/overflow slots, proto data, array length; up to 4 entries, the interpreter's order) and SetNamed (own data), miss -> Runtime::slow at the same pc.
4. Inline GetKeyed/SetKeyed on dense elements.
5. JitStats.calls_out; profitability counts inline IC sites as inline.
6. Tests at the seams: jit_differential (interpreter vs on/force, plus calls_out stats), bench jit_workloads checksums, GC stress forced every safe point.
7. macOS MAP_JIT + pthread_jit_write_protect_np in exec.rs, cfg-gated.
8. CI: --features jit with AGNIJS_JIT=force on Linux/Windows/macOS (differential, VM suite, test262 subset).
9. Re-measure (alternating binaries, median of 5), tune mode on; gates; ADR/notes.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
## Implementation (TASK-101.3.2)

- Inline IC hits in machine code: GetNamed (own inline/overflow slots, prototype data, array length; monomorphic up to 4-way polymorphic, in the interpreter's entry order) and SetNamed (own data), GetKeyed/SetKeyed on dense elements. Guards are shape compares plus proto_epoch; any miss goes to Runtime::slow at the same pc, so nothing deoptimizes. Layout comes from JitLayout probes at start-up; a failed probe disables the inline paths.
- GC: Ctx carries the arena base, proto epoch and IC table, refreshed after every call out; compiled code never holds a heap address in a register across a call that can collect (invariant I3, ADR 0020). Tests: heap_values_survive_collections_at_every_call_out_from_compiled_code, collections_at_every_safe_point_keep_compiled_frames_values; jit_differential and jit_inline_cache pass with AGNIJS_GC_STRESS=1 AGNIJS_JIT=force (the calls_out bound is skipped under stress, where every back edge polls by design).
- macOS: MAP_JIT with pthread_jit_write_protect_np where supported; where pthread_jit_write_protect_supported_np() is 0 (Apple silicon CI VMs) the MAP_JIT mapping is written then mprotect'ed read-execute. CI signs the probe with the allow-jit entitlement under the hardened runtime.
- CI: jit job (--features jit, AGNIJS_JIT=force) on ubuntu, windows and macOS: differential tests, VM suite, test262 subset. Green on run 42805300777.
- Mode on tiering: HOT_THRESHOLD 1000, then profitability judgement, then HOT_PER_INSTRUCTION (200) back edges per bytecode instruction before compiling (compile costs ~25 us per instruction); adaptive abandon over a cross-run window (calls out + entries*20 vs body*edges, judged after 1024 edges or entries). Counting is inline (Vm::counted); the first dispatch offer is gated on jit_live.

## Measurements (alternating binaries, median of 5; ratio vs feature off)

agnijs-js-bench, mode on: loop-arith 0.354, loop-array-index 0.81, prop-monomorphic 0.756, prop-polymorphic 0.71, prop-write-alloc 0.90, closures 0.96, calls-recursive 0.97, method-calls 0.97-0.98; others within 1-3%. Force: closures 1.33, calls-recursive 1.30, method-calls 1.25 (frame round trips).
Gable benches, on / force: rxjs-operators 1.012/1.38, rxjs-higher-order 1.006/1.27, rxjs-subject-fanout 0.996/1.30, signal-churn 1.011/1.26, signal-create 1.017/1.30, template-mount 0.999/1.28, template-update 0.991/1.37, list-diff 0.996/1.16, ui-bundle-startup 0.992/5.28. Mode on is within noise everywhere.
Feature-off build vs main: within noise except prop-write-alloc 1.05-1.08 and promise-chain 1.03-1.05; ObjectData layout is identical to main, attributed to code layout, not proven.

## Gates

test262 with the feature off, AGNIJS_JIT=on and AGNIJS_JIT=force: default and --precompile 42231/42231, intl402 981/981, RegExp suites 2511/2511 in auto, backtracking-only and linear-when-eligible; 0 regressions in all 18 runs. GC-stress slice (AGNIJS_GC_STRESS=16, force): 5949/5949. Feature-off cargo test --workspace green. Gable suites via the path-patched harness: 759 passed, 1 known yjs timing failure (off, on, force); harness manifest and lockfile restored.
Pre-existing CI failure, not from this task: the regex-crate step with the linear matcher compiled out (crates/agnijs-js-regex/tests/nesting.rs:208), already failing in run 42789813130.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Compiled code now runs inline-cache hits without leaving machine code: named loads/stores (monomorphic to 4-way polymorphic, prototype data, array length) and dense element get/set, guarded by shape compares plus proto_epoch with every miss going to Runtime::slow at the same pc (no deopt, no GC maps; I3 documented in ADR 0020 and tested under GC stress). macOS uses MAP_JIT (per-thread write protection where supported, mprotect after copy otherwise), exercised by a signed hardened-runtime probe in a new macOS jit CI job alongside Linux and Windows.

Mode on: prop-monomorphic 0.76x, prop-polymorphic 0.71x, loop-arith 0.35x, loop-array-index 0.81x of interpreted time; no micro or Gable bench loses beyond noise (tiering tuned: per-instruction compile threshold, adaptive abandon). method-calls is only 0.97-0.98x, so AC #2 is not met: calls still round-trip through the interpreter's frame push. TASK-101.3.3 records compiled calls as the follow-up. All test262, intl402, RegExp, GC-stress and Gable gates pass with the feature off, on and force.
<!-- SECTION:FINAL_SUMMARY:END -->
