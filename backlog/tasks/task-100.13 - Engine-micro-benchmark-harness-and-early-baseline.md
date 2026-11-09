---
id: TASK-100.13
title: Engine micro-benchmark harness and early baseline
status: Done
assignee:
  - '@claude'
created_date: '2026-11-09T00:15:00Z'
updated_date: '2026-11-09T00:33:00Z'
labels:
  - ready-for-agent
dependencies: []
references:
  - docs/js-engine-spec.md
  - docs/adr/0016-the-engine-workspace-stands-alone.md
parent_task_id: TASK-100
ordinal: 225000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Spec section 11 says micro-benchmarks should be kept as each engine area lands and are only formalised in M9 (TASK-100.10), but none exist, so there are no speed or memory numbers for M0 to M7. Add a small micro-benchmark harness under native/engine (Agni-agnostic per ADR 0016: no Gable vocabulary or dependencies) that runs the same JS workloads on the owned engine and the QuickJS-NG adapter and records a rough baseline. Separate from, and not a substitute for, the M9 Gable benchmark set and tolerance.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A benchmark harness in native/engine runs identical JS micro-workloads (loops, property access and inline-cache hits, string building, array and Map/Set operations, closures and calls, regex, JSON, promise/job throughput, bytecode load versus compile-from-source, engine startup) on both the owned engine and the QuickJS-NG adapter
- [x] #2 It reports time and peak memory per workload and per engine, and can write results to a machine-readable file for later comparison
- [x] #3 A first baseline with machine details is committed or documented, with the slowest relative results called out as a short findings note
- [x] #4 The harness is not part of the default cargo test run and the boundary tests (ADR 0016) still pass
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. New workspace member crates/gable-js-bench (bin bench plus a lib for the testable parts). Depends only on gable-engine-api, gable-engine-quickjs, gable-js-vm and already-locked crates (serde, serde_json, clap, anyhow); on Windows only, windows-sys 0.61 (already in Cargo.lock via clap) for the process peak working set; Unix uses getrusage. No edits to gable-js-vm or other crates beyond the workspace members list and the lockfile.
2. Workload registry: each workload is setup JS plus run JS returning a checksum string, sized by a SCALE factor; special kinds for engine startup, compile-from-source and bytecode load. Categories per AC1. Only ECMAScript built-ins (no host globals).
3. Runner: the parent spawns itself once per (engine, workload) pair so peak RSS is isolated per pair; the child does warmup plus N measured iterations (run JS then run_jobs) and reports samples, checksum, engine-reported heap bytes and process peak RSS as JSON on stdout.
4. Results file (schema engine-bench-1): machine details (os, arch, cpu model, logical cpus, rustc -V, label), config, per-pair samples and summary. bench compare OLD NEW prints per-pair ratios of medians and flags changes beyond a threshold. The run report prints the owned/QuickJS ratio per workload.
5. Test seams (chosen without user input, TDD): (a) registry: unique names, every AC1 category present, filter selection; (b) stats summary (min/median/mean/max) against literal values; (c) result file JSON round trip and schema-tag rejection; (d) comparison ratios, missing pairs and threshold flags; (e) smoke: every workload at a tiny SCALE runs on both engines and their checksums agree. Timing and peak-RSS measurement are not unit-tested. Benchmarks never run under cargo test; only these fast tests do.
6. Release-mode baseline on this machine (3 repeated runs to gauge noise); commit the results JSON under crates/gable-js-bench/baselines/ and a findings note calling out the slowest ratios. No engine tuning.
7. Verify cargo test --workspace --locked, the boundary tests and a standalone export build.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
- Crate native/engine/crates/gable-js-bench (bin bench: run, report, compare, list). Each (engine, workload) pair runs in a child process of the bench binary so the peak working set / RSS belongs to that pair; times are per-iteration wall clock (std::time::Instant), median reported. Child processes print JSON; the parent writes an engine-bench-1 results file with machine details.
- 20 workloads across all AC1 areas; only ECMAScript built-ins. Every workload returns a checksum and bench run fails if the engines disagree.
- Peak memory: Windows via windows-sys 0.61 K32GetProcessMemoryInfo (windows-sys was already in Cargo.lock through clap; one commented unsafe call in the bench crate only), Linux via /proc/self/status VmHWM, macOS via getrusage. Linux and macOS paths were not compiled here (no cross toolchain); CI build-test on those OSes will exercise them.
- TDD seams: stats summary, workload registry/selection, results file round trip and schema rejection, comparison (ratios, threshold verdicts, missing pairs), plus tests/workloads.rs smoke test (every workload at scale 0.01 on both engines, checksums agree; promise-chain checksum equals the literal 1200). Timing is not tested.
- Baseline: three release runs on Windows 11 / i9-9900KF; run 1 committed as baselines/2026-11-windows-x86_64-i9-9900kf.json, findings and noise in baselines/README.md. Geomean owned/QuickJS 2.87x; worst loop-array-index 7.06x, prop-polymorphic 6.98x, prop-monomorphic 6.33x, engine-startup 6.22x, calls-recursive 5.53x; map-set is 0.23x (owned faster). Owned engine peak memory much higher on allocation-heavy work (promise-chain 145 vs 22 MiB, array-methods 91 vs 5.6 MiB). Run-to-run median drift mostly under 3%, max 10.6%.
- Verified: cargo test --workspace --locked passes; boundary tests pass; git archive HEAD:native/engine export builds and tests offline with --locked.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added gable-js-bench, an engine micro-benchmark harness under native/engine that runs 20 identical JS workloads (loops, property access and IC sites, strings, arrays, Map/Set, closures and calls, regex, JSON, promise and async throughput, compile-from-source versus bytecode load, engine startup) on the owned engine and the QuickJS-NG adapter through the engine interface. It reports median time and per-pair process peak memory plus engine-reported heap, verifies both engines compute the same checksum, writes engine-bench-1 JSON results and compares two results files with a noise threshold.

First baseline (Windows 11, i9-9900KF, release) is committed with a findings note in crates/gable-js-bench/baselines/README.md: the owned engine is 2.87x slower than QuickJS-NG by geometric mean, 4x to 7x on interpreter-core workloads (loops, property reads, calls, startup, bytecode load), closest on JSON and strings (1.2x to 1.7x), faster only on map-set (0.23x), and holds much more memory on allocation-heavy work (GC pacing).

Verification: unit tests and a small-scale checksum smoke test (cargo test), cargo test --workspace --locked, boundary tests, and an offline locked build/test of the git-archive export. No engine tuning was done. Risks: the Linux and macOS peak-memory paths were not compiled locally; numbers are from a non-isolated desktop machine.
<!-- SECTION:FINAL_SUMMARY:END -->
