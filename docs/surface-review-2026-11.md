# Embedder-surface and resource-limit review, November 2026

A defensive review of AgniJS outside the JIT, at commit `fea680f` (origin/main, 2026-11-13), on Windows 11 x64 with rustc 1.99.0. It covered:

- memory safety;
- the embedder boundary: the C API, the Rust API, the resolver, import maps and CLI file access;
- the loaders and endpoints: bytecode, snapshots, module loading, source maps and CDP;
- the resource limits: memory, string length, stack, interrupt and fuel.

The JIT was reviewed separately (`docs/jit-review-2026-11.md`).

The review found 19 defects. They are filed as TASK-103.37 to TASK-103.55 and are not fixed here. None is a memory-safety defect: no out-of-bounds access, use-after-free or undefined behaviour was found. The worst are one capability bypass in the resolver and a set of ways for untrusted script to crash, abort or hang the host process, or to get past the memory limit.

Finding nothing in an area is not proof that the area is sound. [What was not reached](#what-was-not-reached) lists what this review did not cover.

## Findings

Every finding below was reproduced twice. The first run was on the working debug build. The second was on a clean debug build: the target directory was deleted and rebuilt at `fea680f`. Each task gives the minimal repro, the exact command, the root cause, a suggested fix and the test that should catch it.

Severity scale (from the brief):
- Memory unsafety: critical.
- Capability bypass: high.
- Panic, crash, hang or limit bypass reachable from untrusted script: medium.
- Minor: low.

The backlog priority is in brackets where it differs from the severity.

| Task | Severity | Status | What |
| --- | --- | --- | --- |
| TASK-103.38 | Capability bypass [medium priority] | Fixed in d914320 (was confirmed: Rust test) | The resolver's segment checks split on `/` only, but the join turns `\` into `/`. So `pkg/sub/..\..\x` escapes the package through exports patterns, imports patterns (`#internal/..\..\x`) and legacy subpaths, on every platform. The forward-slash forms are refused. |
| TASK-103.40 | Capability bypass, low | Fixed in dadf526 (was confirmed: Rust test, 201 s) | C API engine tags are 16 bits and wrap after 65,535 engines. A live engine then accepts another live engine's value and realm ids: status 0, the other engine's value is read, and `free` succeeds. |
| TASK-103.37 | Medium | Fixed in caa2984 (was confirmed: CLI, exit 101) | The CLI's `data:` sourceMappingURL percent-decoder slices a `&str` at a byte offset. A multi-byte character after `%` panics, in a script or in a module loaded by `import()`. |
| TASK-103.39 | Medium | Fixed in 9203079 (was confirmed: Rust test, abort 0xc0000409) | Rust-API conversion of a sparse array preallocates from the JS `length`. `[]` with length 2^32-1, passed to a registered function or returned to `eval::<serde_json::Value>`, aborts the process with a 128 GiB allocation. |
| TASK-103.41 | Medium | Fixed in 977e84d (was confirmed: CLI, exit 101) | `TypedArray.prototype.copyWithin` panics when an argument's `valueOf` shrinks a resizable buffer. |
| TASK-103.42 | Medium | Fixed in 977e84d (was confirmed: CLI, exit 101) | `ArrayBuffer.prototype.slice` panics (arraybuffer.rs:386) when the buffer shrinks during argument conversion or in the species constructor. |
| TASK-103.43 | Medium | Fixed in 977e84d (was confirmed: CLI, exit 101) | Atomics operations panic (atomics.rs:99) when an index or value conversion shrinks a length-tracking view. |
| TASK-103.44 | Medium | Fixed in 74a0c56 (was confirmed: CLI, abort) | `BigInt.asIntN` / `asUintN` with `bits` near 2^53 abort the process with a 1 PB allocation. Below that, the result is not counted against the memory limit. |
| TASK-103.46 | Medium | Fixed in b5f79bb (was confirmed: CLI, 0xc00000fd) | Deep left-associative chains (binary, member, call and others) overflow the native stack when the syntax tree is dropped, after the parser accepted them. |
| TASK-103.47 | Medium | Fixed in 771fff6 (was confirmed: CLI, 0xc00000fd (debug build)) | IsArray, IsConstructor and GetFunctionRealm recurse through proxy and bound-function chains without a stack check. |
| TASK-103.48 | Medium | Fixed in 74a0c56, 1100ba9 (was confirmed: CLI, abort) | `apply`, `Reflect.apply` and `Reflect.construct` preallocate an argument list from an attacker-chosen `length` (16 TiB). |
| TASK-103.49 | Medium | Fixed in 74a0c56 (was confirmed: CLI, abort) | `RegExp.prototype[Symbol.replace]` preallocates captures from a custom `exec` result's `length`. |
| TASK-103.50 | Medium | Confirmed: CLI | The collector counts each live BigInt as 48 bytes. 1,000 BigInts of 2 MiB stay alive under `--max-memory 16M` (2,019 MiB peak, exit 0). |
| TASK-103.51 | Medium | Confirmed: CLI | `payload_bytes` is 0 for FinalizationRegistry cells, DisposableStack resources and bound-function arguments. These reach 3.0 GiB, 2.5 GiB and 330 MiB under a 16 MiB limit with no out-of-memory error. |
| TASK-103.53 | Medium | Confirmed: CLI | Native loops that allocate from a bare length or a native iterator never check the limit. Under 16 MiB, peaks reach 525 MiB to 3.6 GiB before exit 3. The loops are `fill`, Promise combinators, spread, `Intl.ListFormat` and typed-array `entries`. |
| TASK-103.55 | Medium | Confirmed: CLI | BigInt parsing, division, `toString` and `**` are superlinear and never poll the interrupt. Parsing ran past 120 s, one division took 51 s, and `3n ** 100000000n` ran past 40 s. |
| TASK-103.52 | Low-medium [low priority] | Confirmed: CLI | The regex compile cache and large compiles amplify memory: 525 MiB under 16 MiB with nothing kept. |
| TASK-103.54 | Low-medium [low priority] | Confirmed: CLI | `toHex` and `toBase64` build the whole result (2.57 GiB) before the string-length check throws. |
| TASK-103.45 | Low | Fixed in 977e84d (was confirmed: CLI) | `ArrayBuffer.prototype.transfer` ignores `maxByteLength`: the result has `byteLength` 100 with `maxByteLength` 8. |

Two kinds of failure are worth separating:

- **Panics (exit 101).** The C API's `catch_unwind` guard turns these into errors for C embedders, but a Rust embedder or the CLI goes down.
- **Allocation-failure aborts (0xc0000409 or exit 127) and stack overflows (0xc00000fd).** Nothing can catch these. TASK-103.39, 103.44, 103.46, 103.47, 103.48 and 103.49 kill any embedding, the C API included.

## Scope

- **Memory safety outside the JIT.** All `unsafe` code in the workspace outside `agnijs-js-jit` is in `agnijs-capi`; the engine, `agnijs`, the CLI and the derive crate are `#![forbid(unsafe_code)]`. Read in full:
  - `crates/agnijs-capi/src`: `lib.rs`, `table.rs`, `ptrs.rs`, `types.rs`, the lending stack (`lend`, `Pop`) and the owner registry;
  - `include/agnijs.h` checked against `types.rs`;
  - the `agnijs-engine-api` `Handle` and its release queue.
- **The embedder boundary.**
  - The Rust API (`crates/agnijs`: `convert.rs`, `context.rs`, its panic sites, and the job loop in `Context::turn`).
  - The C API id scheme and wrong-engine checks.
  - `agnijs-js-resolve`: exports, imports, legacy packages and path joining.
  - The WHATWG import map.
  - CLI file and module access, and its timers (`crates/agnijs-cli/src/host.rs`).
- **Loaders and endpoints.**
  - The bytecode and snapshot loaders.
  - The debugger's source-map parser.
  - The CDP WebSocket server.
  - `data:` sourceMappingURL handling in the CLI.
- **Resource limits.**
  - `--max-memory` / `memory_limit` accounting: `Class::payload_bytes`, the collector's size model, large native allocations.
  - The string-length limit.
  - Stack depth: the parser, the compiler, AST drop, and native recursion.
  - Interrupt and fuel polling in native loops and BigInt arithmetic.
- **Known issues.** These were read first and not re-reported: `docs/security-model.md`, ADRs 0010, 0013 and 0022, and TASK-103.24 to 103.33.

## Method and run counts

- **Code reading** of the files above, looking for: preallocation from script-controlled lengths; slices taken after a user callback could resize a buffer; recursion without a depth check; loops without interrupt or memory polls; and string slicing at byte offsets.
- **Targeted probes.** Three parallel passes wrote small scripts and ran them against the debug CLI under `--max-memory`, measuring peak working set:
  - typed arrays, ArrayBuffer and Atomics re-entrancy: about 1,500 sub-cases;
  - limits and recursion: about 230 probes;
  - the embedder boundary: Rust and C tests.
- **Miri** (nightly) on `agnijs-capi`:
  - the 5 `miri_` unit tests (table and pointer checks): pass;
  - a nested-lending re-entry test (host function calls into JS, which calls another host function): pass, 63 s;
  - the `hostile` and `basics` integration tests: all pass, with two exceptions. `another_thread_can_interrupt_a_runaway_regex_and_a_native_loop` fails because it asserts a 5 s wall-clock deadline that Miri cannot meet; this is a timing artifact, not a defect. `the_memory_limit_counts_buffers_and_collections_and_refuses_huge_requests` was cut by the 2,700 s timeout, so it was not reached under Miri.
  - Miri also warned about an integer-to-pointer cast in the test code of `host_objects_carry_data_and_are_finalized`.
- **Fuzzing** with the in-tree harnesses, all clean:
  - snapshot loader: 2 seeds × 10,000 inputs (4,384 loaded and ran the probe);
  - bytecode loader at default iterations with seed B, plus seed C at 4,000 iterations:
    - script: 3,405 of 5,500 runs loaded;
    - module: 2,954 of 4,600 loaded;
    - snapshot code: 802 of 4,400 loaded;
  - C API fuzz: 20 runs.
  - A further C API run under `AGNIJS_GC_STRESS` was stopped by its 1,800 s timeout before finishing, so it gives no result.
- **Re-verification.** All repros were re-run from a clean build at `fea680f` before filing.

## Reviewed with no finding

- C header against `types.rs`: sizes, versions and enum values agree.
- `ptrs.rs` input and output checks: null, alignment, length and encoding. Miri passes.
- C API id table: stale ids, ids of another kind and ids from another table are refused. The 16-bit tag wrap is the only gap found (TASK-103.40).
- The lending stack under nested re-entry (Miri) and the `Handle` release queue (no overlapping borrow).
- Rust API panic sites; re-entry from a getter during conversion is caught as a `RangeError` and does not crash.
- The import map (it uses the `url` crate and follows WHATWG).
- The debugger's source-map parser (hardened against malformed VLQ and bounds).
- The CDP server's frame parsing.
- The bytecode and snapshot loaders (code reading and fuzzing above).
- CLI timers: the rest-argument array passed to `setTimeout` is dense, so the sparse-array conversion abort (TASK-103.39) does not reach it.

## Plausible, not filed

These were looked at but not confirmed as defects, or are spec-mandated:

- `Context::turn` takes all tasks and returns early with `?` part-way through, which looks like it could drop pending tasks. Repros at 256 KiB and at 4 KiB above usage both settled every task, so it is not filed.
- Iterator close after an interrupt may still run `return()`.
- `Intl.Segmenter` may be O(n²) on some inputs.
- `Date.parse` is lenient about some formats.
- `0n << 2n**64n` diverges from the spec in its error.
- CDP: unbounded threads per connection, no read timeout, and a 64 MiB frame cap. These are only reachable by someone who can already connect to the debugger.
- `request_interrupt` on an idle engine stays set until the next run.
- A theoretical C API `put` failure after the lend guard is armed.
- Long loops that the spec itself requires, such as very large `Array.prototype` operations on array-likes.

## What was not reached

- **`agnijs-engine-quickjs`.** FFI through rquickjs was not reviewed.
- **Differential testing against QuickJS.** No QuickJS binary was available.
- **Release builds.** Every confirmation is on a debug build. TASK-103.47 (native recursion through chains) may not reproduce in release, where frames are smaller. Its acceptance criteria ask for a fix that holds in both.
- **Sanitizers** (ASan, TSan) were not run. Miri covered only `agnijs-capi`, and two of its tests did not complete.
- **`Intl.NumberFormat` and `Intl.DateTimeFormat`** could not be exercised: the build has no ICU data for `en`.
- **The JIT** was out of scope (see `docs/jit-review-2026-11.md`).
- **Long fuzzing.** Fuzzing was bounded to share the build lock with another agent. There was no coverage-guided fuzzing (cargo-fuzz or libFuzzer).
