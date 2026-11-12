---
id: TASK-103.30
title: >-
  Bytecode and snapshot loading: verify context-dependent operands so crafted
  input cannot panic the VM
status: To Do
assignee: []
created_date: '2026-11-12T06:25:00Z'
labels:
  - ready-for-agent
  - security
dependencies: []
references:
  - docs/security-model.md
parent_task_id: TASK-103
priority: medium
ordinal: 294000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the security review (TASK-103.9, docs/security-model.md). ADR 0010 (M7 addendum) and ADR 0022 trust operands whose validity depends on run-time context (environment depth and slot, module import slots): the loader checks only what indexes a block's own pools. The checksum is FNV-1a and not a signature, so anyone who can write the bytes can reseal them. Crafted bytecode can then reach VM invariants that are `expect`s or indexing (for example `unwind_env_to` in `interp/mod.rs` expects the environment chain to match the frame's depth). TASK-103.24 fixed one such panic for module imports, and ADR 0022 made `LdaEnv`/`StaEnv` fall back instead of panicking. The tests only load mutated code, they do not run it (`fuzzed_snapshots_load_or_fail_without_panicking` in `agnijs-js-vm/tests/snapshot.rs`; `corrupted_bytes_never_panic` in `agnijs-js-compiler/tests/serialize.rs`), except the C API fuzzer, whose mutations nearly always fail the checksum. In the Rust API such a panic unwinds into the embedder; in the C API it poisons the engine. ADR 0022 names a bytecode verifier as what would close both loaders.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Loading bytecode or a snapshot verifies every operand the interpreter or the JIT relies on (environment depth and slot, import slots, handler and generator state), or the interpreter checks it at run time, so that running any container that passes the loader cannot panic
- [ ] #2 A fuzz test mutates bytecode and snapshots, reseals the checksum, and runs the result (with and without the jit feature) without a panic
- [ ] #3 ADR 0010 and ADR 0022 record what is verified and what is still trusted
<!-- AC:END -->
