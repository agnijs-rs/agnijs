# 20. The baseline JIT generates code with Cranelift's code generator alone, never speculates, and leaves compiled code for every call

Date: 2026-11-10

## Status

Accepted

## Context

ADR 0019 proposes Cranelift (`cranelift-codegen`, `cranelift-frontend`, `cranelift-jit`) for the baseline JIT and asks TASK-101.3 to measure it before committing. It also fixes the constraints a tier must keep: one frame format (J1), atomic instructions (J3), a precise collector with no stack scanning (J6), one attention word (J7), faults as values (J8), the debugger forcing the interpreter (J9), and `unsafe` confined to `agnijs-js-jit` behind an optional `jit` feature (J10).

Two pieces of a JIT are reserved for later work and are not built here: deoptimization (side exits) and GC maps (frame-state reconstruction). Until they exist, compiled code must never need either one.

This ADR records the backend decision, as checked against the repository's rules, and the design of the tier built under those limits.

## Decision

### Backend: Cranelift 0.137.1, code generator only

The baseline JIT depends on `cranelift-codegen`, `cranelift-frontend` and `cranelift-native`, pinned to `=0.137.1`, with default features off. It does not use `cranelift-jit`, `cranelift-module` or `cranelift-object`. The proposal in ADR 0019 stands except for `cranelift-jit`, which is replaced by a small module of our own (`agnijs-js-jit/src/exec.rs`). The reasons:

- **Compiled functions need no linking.** A function's code has no relocations: helpers are reached through function pointers in a context block, and constants are immediates. `cranelift-jit`'s symbol resolution, GOT and PLT would add code and `unsafe` that nothing uses.
- **Executable memory stays ours.** It is mapped read-write, filled, then switched to read-execute: `VirtualAlloc`/`VirtualProtect` with `FlushInstructionCache` on Windows, `mmap`/`mprotect` on Unix. It is never writable and executable at once. A probe at first use maps a page, makes it executable and runs a one-instruction function. If the probe fails, for example under Arbitrary Code Guard or a hardened policy, the JIT reports itself unavailable and the interpreter runs (ADR 0019, Platforms).
- **No unwind tables.** Compiled code never unwinds. A panic in a helper is caught in its trampoline, which returns a "panic" exit; the panic is then resumed in Rust after the machine-code frame is gone. Cranelift's `unwind` feature is off.

`cranelift-object` stays the route for AOT (TASK-101.5); nothing here rules it out.

The backend was checked against the repository's rules:

| Rule | Result |
| --- | --- |
| Licenses (`scripts/check-licenses.mjs`) | Passes. Cranelift, `regalloc2`, `target-lexicon` and `wasmtime-internal-core` are Apache-2.0 WITH LLVM-exception, which is on the allowed list. The other new crates (`gimli`, `bumpalo`, `hashbrown`, `arbitrary`, `fnv`, `rustc-hash`, `mach2`, `libc`) are MIT or Apache-2.0. `THIRD-PARTY-NOTICES` names them. |
| Pure Rust | Yes. No C or C++ is built. `cranelift-codegen`'s build script runs the ISLE generator in Rust. |
| `unsafe` confined | `agnijs-js-jit` is `#![deny(unsafe_code)]`. Only `exec` (executable memory, the entry trampoline, the helper trampolines) allows it. Every other engine crate keeps `#![forbid(unsafe_code)]`, and `agnijs-js-vm` still forbids it with the feature on. |
| Off by default | `jit` is a feature of `agnijs-js-vm` (re-exported by `agnijs-js-test262` and `agnijs-js-bench`). Without it, the dependency graph and binaries are unchanged. |
| Binary size | Release builds with debug info: `test262` is 23.7 MB without the feature and 29.2 MB with it; `bench` is 23.3 MB and 28.7 MB. So the feature adds about 5.5 MB. Most of that is Cranelift's x64 backend and its debug info. Only the host architecture's backend is built (`host-arch`). |
| Build time | On the reference machine (i9-9900KF), a clean release build of `agnijs-js-jit` and its Cranelift dependencies takes about 100 s once the engine crates below it are built. `cranelift-codegen` dominates, and in a parallel workspace build part of it overlaps the other crates. After that, incremental rebuilds of the VM take about 20 s with or without the feature. |
| Feature-off cost | None. The tiering state (`Feedback::tier`) exists only under `agnijs-js-compiler/tier`, which `jit` enables, and every hook in the dispatch loop is `cfg`-gated. A full `bench run` without the feature matches the pre-JIT build within noise when both are built from the same path. Builds from different checkout paths differ by up to 10% on the call workloads, through code layout alone. |
| Toolchain | Cranelift 0.137 needs Rust 1.97. The crate declares that version, and the rest of the workspace keeps its own minimum because the crate is optional. |
| WASM, iOS, other architectures | `build.rs` sets `cfg(jit_host)` only for x86-64 and AArch64 on Windows, Linux, macOS and Android. Cranelift is a dependency only on those targets, through the same target table. Elsewhere the crate builds without a code generator, `available()` is false, and `compile` returns an error, so the interpreter runs. The wasm32 target is not installed on the build machine, so the wasm32 build was not checked here; CI should add a `cargo check --target wasm32-unknown-unknown -p agnijs-js-jit`. |
| macOS | Not yet as ADR 0019 describes. The mapping is plain `mmap` followed by `mprotect`, with no `MAP_JIT` and no `pthread_jit_write_protect_np`. Under the hardened runtime the probe fails and the interpreter runs, which is safe. Apple Silicon support with the entitlement is a follow-up. |

Compile latency is small. With every eligible block forced, the benchmark workloads compile 5 to 8 blocks each in 2 to 5 ms in total (`JitStats::compile_time`), which is 0.4 to 0.6 ms and 600 to 1,300 bytes of machine code per block. In mode `On`, blocks compile only after 1000 entries and back-edges. That is far below ADR 0019's "a fifth of the gain" bar wherever there is a gain.

### Design: a baseline that needs neither deoptimization nor GC maps

- **Per-block compilation, in the interpreter's frame.** A `CodeBlock`, a function or script body, compiles to one machine function `fn(ctx) -> exit`.
  - The register window is read and written in memory, through a pointer in the context that is reloaded after every call out. So a register file that reallocates is fine (J1).
  - The accumulator lives in Cranelift variables and is flushed to the context before every call out.
  - Compiled code can be entered at any *entry pc*: pc 0, every back-edge target (on-stack replacement), and every pc where the runtime can continue after handling an instruction.
- **What compiles inline.** The following are emitted as machine code, each behind a tag guard whose failure runs the instruction's generic semantics instead:
  - constants, `Ldar`, `Star`, `Mov`;
  - numeric arithmetic, including `%` and `**` through pure helpers;
  - the int32 bitwise operators and shifts;
  - comparisons and the fused compare-and-branch forms;
  - truthiness and tag branches, `Inc`, `Dec`, `ToNumeric`, `CheckTdz` and jumps;
  - attention and interrupt polls at back-edges (J7).

  The guards cannot exit compiled code. A failed guard is not a side exit, so there is nothing to deoptimize.
- **Everything else calls the runtime, one instruction at a time.** `Runtime::slow(pc, acc)` writes the frame back, then tries the dispatch loop's own fast paths: inline-cache hits on named and keyed property access, cached globals, environment slots, cached string constants and strict equality. Otherwise it runs the instruction's generic `step`. The result is either "continue compiled code at this entry pc with this accumulator" or an exit. Because instructions are atomic (J3), the frame is an interpreter frame at an instruction boundary at every call out.
- **Calls leave compiled code.** A call pushes the callee's frame and exits (`Exit::Frame`). The VM's tier loop (`Vm::jit_run`) then enters the callee's compiled code, if it has any, or interprets it. A return pops the frame and re-enters the caller at its continuation pc. Machine code therefore never recurses on the Rust stack. That keeps the native-depth accounting and the birth-serial retention bound (TASK-100.16) exactly as under the interpreter.
- **Exits.** `Return`, `Frame`, `Fault` (handled by the interpreter's handler lookup at the written-back pc, J8), `Done`, `Interp` and `Panic`. `Interp` is taken when the attention word or the debugger needs the interpreter's loop head, so a debug client forces interpretation (J9).
- **Roots.** Every heap value compiled code can hold across a call out is in the register file or the frame. So a collection under compiled code finds its roots exactly as it does under the interpreter, with no GC maps and no stack scanning (J6).
- **Tiering.** `JitMode` is `Off` (the default), `On` or `Force` (compile every eligible block on first offer; for tests). It is set by `AGNIJS_JIT` or by `Vm::set_jit_mode`.
  - Blocks are offered at function entry, after a call returns, and at loop back-edges. The counters live in the per-block feedback side table (`Tier`), as J4 requires.
  - A block the tier does not handle is marked never-compile. `eligible` rejects these blocks before any code is generated:
    - generators and async functions;
    - derived class constructors;
    - module and eval code;
    - blocks containing direct `eval`, `super()` or `debugger`;
    - blocks of more than 20,000 instructions.
  - In mode `On`, a hot block is compiled only if it is *profitable*: at most one in six instructions calls out, counting a call as two. Otherwise it is left to the interpreter, because each call out costs more than the interpreter's own dispatch.
  - A block marked never-compile costs one test of a local in the dispatch loop.

### Measured (TASK-101.3)

Release build, i9-9900KF, Windows 11, median of 5 iterations, `bench run --filter loops --filter property-access --filter calls`. Ratios are JIT time over interpreter time.

| Workload | Interpreter | `AGNIJS_JIT=on` | `AGNIJS_JIT=force` |
| --- | --- | --- | --- |
| loop-arith | 104.9 ms | 35.3 ms (0.34) | 36.2 ms (0.35) |
| loop-array-index | 94.5 ms | 98.1 ms (1.04) | 111.5 ms (1.18) |
| prop-monomorphic | 81.3 ms | 79.3 ms (0.98) | 112.6 ms (1.39) |
| prop-polymorphic | 126.4 ms | 127.8 ms (1.01) | 146.5 ms (1.16) |
| prop-write-alloc | 76.9 ms | 75.2 ms (0.98) | 96.9 ms (1.26) |
| closures | 64.3 ms | 61.5 ms (0.96) | 82.4 ms (1.28) |
| calls-recursive | 54.3 ms | 55.8 ms (1.03) | 68.1 ms (1.25) |
| method-calls | 118.1 ms | 116.7 ms (0.99) | 165.2 ms (1.40) |

Run-to-run noise is about 2%.

- A block that runs inline, such as numeric loops, is about 2.9 times faster.
- A block dominated by property access or calls is 16 to 40% slower when forced. A call out costs a trampoline, a second decode and the frame written back, which together cost more than the dispatch they replace, and every call costs an exit and a re-entry.
- In mode `On`, the profitability rule leaves those blocks to the interpreter, so the cost of the tier itself is within noise.

## Consequences

- The tier is correct by construction against the reference interpreter: every compiled instruction either has an inline path guarded by tags, or runs the interpreter's own code on an interpreter frame. `crates/agnijs-js-vm/tests/jit_differential.rs` and `crates/agnijs-js-bench/tests/jit_workloads.rs` compare the two modes, and the VM's full test suite and test262 pass with `AGNIJS_JIT=force`.
- The speed-up is limited to blocks that run inline. Gains on property-heavy and call-heavy code need work that the current design rules out:
  - inline IC hit paths in machine code, reading the object header at the ADR 0019 J5 offsets and the IC side table;
  - direct calls between compiled functions;
  - values kept in machine registers across instructions.

  Each of these makes a compiled frame hold state the interpreter frame does not, or makes a guard able to fail without a generic path. So each one needs the reserved deoptimization and GC-map work first, specified in `docs/design/jit-deopt-and-gc-maps.md` with the stub interfaces in `agnijs-js-jit/src/handoff.rs`.
- ADR 0019's "Code generator" section is refined: `cranelift-jit` is not used, and its macOS row is not yet met (no `MAP_JIT`), so macOS falls back to the interpreter.
- The JIT stays off by default, and spec section 1 ("no JIT" in the default build) still holds.
