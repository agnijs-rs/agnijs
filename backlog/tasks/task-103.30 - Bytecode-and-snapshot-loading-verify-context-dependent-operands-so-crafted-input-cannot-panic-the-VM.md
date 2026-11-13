---
id: TASK-103.30
title: >-
  Bytecode and snapshot loading: verify context-dependent operands so crafted
  input cannot panic the VM
status: In Progress
assignee:
  - '@claude'
created_date: '2026-11-12T06:25:00Z'
updated_date: '2026-11-13T01:30:00Z'
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

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Red: tests/bytecode_fuzz.rs mutates compiled script and module trees (context operands, splices, handlers, block flags, raw tree bytes), reseals them and runs whatever loads; it fails, listing the panic sites.
2. Fix each site: verify module records at decode; turn interpreter expects on register kinds, env chain, suspension and dynamic references into invalid-bytecode throws; make linker unwraps errors; stop the encoder panicking on a refused debug section.
3. Fuzz snapshots: run code from resealed snapshots whose code bytes were altered, in interpreter and JitForce modes.
4. Write ADR 0010 and ADR 0022 addenda and update docs/security-model.md.
5. Gates: clippy, test262 (all modes), GC stress, JIT slices, benchmarks.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Red phase: tests/bytecode_fuzz.rs found 13 distinct panic sites in script code and 6 in module code. They came from register-kind expects, PopEnv underflow, suspending a frame that has no generator, private names, a generator unreachable, the debug-section encoder, forged dynamic-reference env ids, module records missing slots or names, and AsyncFromSyncIterator receivers.

Fixes:
- The decoder verifies module records (verify_module in serialize.rs).
- The interpreter throws 'internal error: invalid bytecode' through Vm::invalid_bytecode, with the or_invalid!/private_name! macros in step.rs, can_suspend in generator.rs and reference_env in globals.rs.
- Linker unwraps are errors.
- afs_parts throws a TypeError.
- unwind_env_to stops at the end of the env chain.
- put_debug_section writes a refused section as empty.

Hang found under JitForce: a handler whose target is inside its own range loops with no back-edge, so Host::interrupt never fired. handle_fault now polls the interrupt on every throw it routes to a handler.

Fuzz modes:
- The JIT is only exercised with the host interrupt (interrupt_after), because fuel keeps compiled code out.
- Debug builds compile on 1 iteration in 50 (BYTECODE_FUZZ_JIT_EVERY), because every block takes about 6 s per case in debug. A new CI step in the jit job runs the fuzz in release with every case compiled.
- GC stress runs a twentieth of the iterations, Miri runs 2.

Verification:
- Seeds 12345 (20k iterations), 1, 99 and 31337 (15k each), debug, interpreter: clean.
- Seed 777, release, jit, 2000 iterations of each test in both modes: clean.
- Miri: the script slice passed (2 cases).
<!-- SECTION:NOTES:END -->
