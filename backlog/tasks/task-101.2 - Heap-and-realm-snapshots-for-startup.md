---
id: TASK-101.2
title: Heap and realm snapshots for startup
status: Done
assignee:
  - '@claude'
created_date: '2026-11-09T01:51:00Z'
updated_date: '2026-11-11T05:41:00Z'
labels:
  - needs-triage
dependencies:
  - TASK-101.1
  - TASK-100.12
parent_task_id: TASK-101
ordinal: 230000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Save and restore a realm or heap after initialization so startup becomes a memory load for every embedder (the UI-bundle startup case in the M9 benchmarks). Cover versioning against the bytecode container (ADR 0010), host objects and handles (ADR 0006), and the debugger.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A snapshot of an initialized realm (built-ins plus an embedder-run bootstrap script) can be written and restored, with a version mismatch rejected
- [x] #2 Startup of a bundle from a snapshot is measured against the cold path with gable-js-bench; test262 and the host-driven tests pass with snapshot-created realms
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Engine::snapshot(realm) default method (Unsupported) plus a Snapshot error variant (Display, C API mapping).
2. agnijs-js-vm snapshot module: deterministic writer over objects/envs/strings/symbols/bigints/code reachable from the realm; natives by a per-process registry index (fingerprinted); refuse host objects/functions, natives with private state, generators, modules, pending jobs and other unsupported classes with an error.
3. Reader: AGJS container tag agnijs-snap-1 tied to the bytecode format, checksum, bounded counts, every id and payload validated; Vm::bare + make_engine_from_snapshot. Code blocks through the serializer once each; feedback and ICs empty; no JIT code.
4. Harden env access so a corrupt snapshot faults instead of panicking; bounded fuzz test.
5. Tests: tests/snapshot.rs through the Engine trait; host_driven in a snapshot variant; test262 --snapshot mode.
6. Bench: snapshot startup and bundle startup cold vs snapshot.
7. Docs (ADR/spec J11), backlog follow-ups for what is left.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
From TASK-101.1 (ADR 0019, J11; spec section 14): snapshot blockers in the current engine are NativeKind::Fn raw function pointers (need a native registry indexed by id), Rc<Cell<bool>>/Rc<Cell<usize>> state shared by NativeData closures (move into the heap or serialise with sharing), and HostObject with no serialisation hook (add one, or refuse with an error). Code blocks go through the existing serializer once each; inline-cache and feedback tables load empty; JIT code is never in a snapshot.

## Delivered (commits 0202f50, 45b1156, 7c9d666, 2252fdd, 8b6f4f6 on main)

- `Engine::snapshot(realm)` (the trait default is Unsupported) and `agnijs_js_vm::make_engine_from_snapshot(host, bytes)`. A new `Error::SnapshotUnsupported(path)` is mapped in the C API registry. Design in ADR 0022.
- Format: an AGJS container with the tag `agnijs-snap-1+<bytecode format>`. Its payload holds a native-registry fingerprint (mismatch: BytecodeMismatch), an FNV checksum over the body (mismatch: BytecodeCorrupt), then the body.
  - Natives are written as an index into a per-process registry built from a fresh realm.
  - Code trees go through the bytecode serializer, once each. ICs and feedback load empty, and JIT code is never written.
- Unsupported state is refused with its path, for example `globalThis.app.hooks.onLoad`:
  - host objects and host functions;
  - natives with captured state (promise resolvers, Promise.all elements, bound Intl methods, revokers);
  - pending reactions;
  - generators, modules, iterators, mapped arguments, FinalizationRegistry and Intl objects;
  - other realms;
  - pending jobs or a running VM.
- The loader validates every count, index and class invariant, and checks that chains are acyclic. LdaEnv and StaEnv no longer panic on a mismatched environment: the slow path throws, in the interpreter and in the JIT helpers.

## Verification

- tests/snapshot.rs: 9 tests through the Engine trait, covering:
  - round trip of a bootstrapped realm;
  - closures, classes and built-in state;
  - script scoping;
  - GC and a second snapshot after a restore;
  - determinism;
  - refusals with their paths;
  - damaged and truncated input;
  - a bounded fuzz of 400 iterations (`SNAPSHOT_FUZZ_ITERS` and `SNAPSHOT_FUZZ_SEED` set it; 3x4000 found no panics);
  - another format version rejected.
  These also pass with `--features jit` and with `--no-default-features --features linear-regex`.
- The conformance kit runs a second time on snapshot-restored engines, all passing.
- host_driven passes 14/14 with `AGNIJS_TEST_SNAPSHOT=1`, and also under `AGNIJS_GC_STRESS=1`.
- test262 passes 42231/42231 in default, `--precompile` and the new `--snapshot` mode, and intl402 passes 981/981. The GC-stress slice passes 5949/5949 in default and `--snapshot` mode. CI now runs the `--snapshot` test262 pass and the snapshot host_driven pass.
- Bench (agnijs-js-bench, release, i9-9900KF), new workloads, one engine per sample:
  - snapshot-startup 0.48 ms against engine-startup 0.59 ms;
  - bundle-snapshot 2.04 ms against bundle-cold 2.79 ms (~100 KB bundle from bytecode plus a first call).
- Hot loop against ca2d7ee: geomeans of 1.0115 and 1.0062 over two full runs. Single workloads move 5-8% in both directions between the runs, which is noise.

## Follow-ups (not created as tasks; awaiting approval)

- C API entry points for writing and restoring a snapshot (bump the ABI minor version).
- A host serialisation hook for HostObject data and host functions.
- Heap-resident state for promise resolver natives and Promise.all elements, so bootstrap code with pending promises can be snapshotted.
- Generator frames, modules and namespaces as snapshot data.
- A native registry generated at build time: the first restore in a process currently costs one realm creation. make_engine also creates two realms.
- A faster restore: map the heap image or share decoded code between engines (the bundle gain is modest because restore still allocates every object and decodes every tree).
- A bytecode verifier, which would close the remaining trust gap for code inside snapshots and for the bytecode loader.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
First slice of realm snapshots.

- `Engine::snapshot(realm)` writes the heap reachable from a realm (built-ins plus whatever the embedder's bootstrap ran) as a versioned, fingerprinted, checksummed AGJS container. `make_engine_from_snapshot(host, bytes)` restores it.
- A build or format mismatch is rejected as BytecodeMismatch.
- Damaged input is rejected as BytecodeCorrupt and never panics (bounded fuzz tests).
- State a snapshot cannot carry is refused with its path.
- test262 (42231/42231), the conformance kit and the host-driven tests pass on snapshot-created realms, and CI runs both.
- agnijs-js-bench measures the startup gain against the cold path: snapshot startup 0.48 ms against 0.59 ms, and the bundle start 2.04 ms against 2.79 ms.
- The hot loop is within noise.

ADR 0022 records the design. Follow-ups (C API, host hook, resolver state, generators and modules, build-time registry, faster restore, verifier) are listed in the notes.
<!-- SECTION:FINAL_SUMMARY:END -->
