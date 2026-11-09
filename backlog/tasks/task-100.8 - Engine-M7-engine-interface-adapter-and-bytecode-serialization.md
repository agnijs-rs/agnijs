---
id: TASK-100.8
title: 'Engine M7: engine-interface adapter and bytecode serialization'
status: Done
assignee:
  - '@claude'
created_date: '2026-11-07T22:36:00Z'
updated_date: '2026-11-09T00:08:00Z'
labels:
  - ready-for-agent
dependencies:
  - TASK-100.7
  - TASK-100.11
documentation:
  - docs/js-engine-spec.md
parent_task_id: TASK-100
ordinal: 209000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The Host runtime only ever sees the engine interface, and production bundles ship precompiled code. Spec sections 3 and 7.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 The Gable engine passes the engine-interface conformance kit
- [x] #2 Bytecode saves and loads (round trip) and a version mismatch is rejected
- [x] #3 Host-driven jobs, timers, interrupt and memory limits and realm creation are exercised by tests
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
Seams under test: (a) the engine interface (Engine::compile_script/compile_module/eval_bytecode, run_jobs, create_realm, set_memory_limit, Host::interrupt) via the conformance kit and engine-level integration tests in gable-js-vm/tests; (b) gable-js-compiler's public serialize API (encode/decode of a compiled CodeBlock), observed by behaviour after decode and by the error returned on bad input.
1. AC1: confirm the conformance kit (21 tests) passes against make_engine; it is already instantiated in gable-js-vm/src/lib.rs.
2. AC2: replace the M2 source-text payload with serialised register bytecode. New gable-js-compiler::serialize module: one instruction table drives encode, decode and a structural verifier (operand indices within their pools, jump targets inside the block, registers below register_count); string table for names; source text stored once (still needed for toString, script_source and positions); inline caches stored as a count only. VM container moves to the shared GBJS container from gable-engine-api::bytecode with tag gable-bc-2; legacy GBLC gable-bc-1 source payloads still load for one release (ADR 0010). load no longer recompiles.
3. Tests: round trip in a fresh engine (functions, closures, classes, generators, async, templates, regexps, BigInt, modules with imports/exports, TLA); mismatch (old tag, other engine tag, garbage) rejected before running; truncated or corrupted payload gives BytecodeCorrupt, never a panic; error locations and Function.prototype.toString survive.
4. AC3: engine-level tests in gable-js-vm/tests/host_driven.rs: jobs drained per realm by the host, host timers firing callbacks that enqueue jobs, interrupt inside run_jobs and inside a module, memory limit in jobs and recovery, realms created from a host function and from bytecode.
5. ADR 0010 addendum; test262 baselines and cargo test --workspace --locked; boundary tests.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Verification (2026-11-08): conformance kit 21/21 against the Gable engine (cargo test -p gable-js-vm --lib conformance). gable-js-compiler/tests/serialize.rs 7/7, gable-js-vm/tests/bytecode.rs 8/8, gable-js-vm/tests/host_driven.rs 14/14. cargo test --workspace --locked: 27 suites green, boundary tests 3/3. test262 --engine gable 42225/42225, --engine gable --precompile 42225/42225 (every harness file, script and module loaded from gable-bc-2 bytes), --only-intl402 981/981, --engine parse --include-intl402 43488/43488; 0 regressions everywhere. Cargo.lock and baselines unchanged (no new dependencies).
Decisions: payload carries a kind byte plus FNV-1a 64 checksum (reverses ADR 0010's no-checksum note; accidental-damage guard, not a signature), because fuzzing the decoded stream reached VM panics (LdaImport outside module) and a native stack overflow via a cyclic array join. Verifier checks operand pools, jump targets, register ranges, handlers, spans, offsets, terminators, nesting <= 256; context-dependent operands (env depth/slot, import slots) are trusted. Legacy GBLC gable-bc-1 still loads for one release. New test262 --precompile mode plus CI step.
VM fix found by AC3 tests: the memory limit was never enforced inside native frames (jobs, callbacks) because collection is deferred there and live_bytes went stale; a for(;;) allocation loop in a .then job grew unbounded. Interp safe point now compares live_bytes + allocated_since_gc against the limit.
Findings not fixed (out of scope): (1) dense element / slot storage growth is not counted in allocated_since_gc, so e.g. new Array(1e7).fill(0) or pushing numbers into one array is not stopped by the memory limit; (2) MAX_NATIVE_DEPTH (1200) assumes a large host stack: String(a) on a self-containing array overflows a default 2MB Rust test thread (test262 runner uses 64MB threads).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Engine M7. gable-js-compiler gains a serialize module (table-driven encode/decode/verify of CodeBlock trees; strings, UTF-16 constants and source stored once; inline caches as a count). compile_script/compile_module now emit gable-bc-2 in the shared GBJS container (kind byte + FNV-1a checksum + tree); eval_bytecode/modules load without recompiling; legacy gable-bc-1 still loads; foreign/future tags are BytecodeMismatch, damaged payloads BytecodeCorrupt, never a panic. test262 runner gets --precompile (CI step added): 42225/42225 from bytecode. New host_driven tests cover jobs, host timers, interrupts (jobs, host functions, modules), memory usage/limits and realm creation; they exposed and fixed the memory limit not applying inside jobs/callbacks. ADR 0010 addendum. Conformance kit 21/21, workspace tests and all test262 baselines green. Follow-ups to consider: count element storage toward the memory limit; host stack size requirement for MAX_NATIVE_DEPTH.
<!-- SECTION:FINAL_SUMMARY:END -->
