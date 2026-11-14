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
| macOS | Since TASK-101.3.2, on Apple silicon, code is mapped with `MAP_JIT` and written with this thread's JIT write protection lifted (`pthread_jit_write_protect_np`); where the host has no per-thread protection (Apple silicon under virtualization, such as CI runners) the `MAP_JIT` mapping is written and then made read-execute with `mprotect`. That is what the hardened runtime with `com.apple.security.cs.allow-jit` permits. If `MAP_JIT` fails, plain `mmap` and `mprotect` are tried, and if the probe still fails the interpreter runs. `exec_memory_kind()` reports which path is in use; CI signs the JIT crate's probe test with the entitlement and checks it. Intel macOS uses `mmap` and `mprotect`. |

Compile latency is small. With every eligible block forced, the benchmark workloads compile 5 to 8 blocks each in 2 to 5 ms in total (`JitStats::compile_time`), which is 0.4 to 0.6 ms and 600 to 1,300 bytes of machine code per block. In mode `On`, blocks compile only after 1000 entries and back-edges. That is far below ADR 0019's "a fifth of the gain" bar wherever there is a gain. (With the inline IC paths of TASK-101.3.2, blocks cost about 25 µs per instruction to compile, so mode `On` waits longer. See the addendum.)

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
- The speed-up is limited to blocks that run inline. Gains on property-heavy and call-heavy code need more work:
  - inline IC hit paths in machine code, reading the object header at the ADR 0019 J5 offsets and the IC side table. Done in TASK-101.3.2 without deoptimization: every guard that fails runs the same instruction's helper at the same pc (see the addendum);
  - direct calls between compiled functions;
  - values kept in machine registers across instructions.

  The last two make a compiled frame hold state the interpreter frame does not, so they need the reserved deoptimization and GC-map work first, specified in `docs/design/jit-deopt-and-gc-maps.md` with the stub interfaces in `agnijs-js-jit/src/handoff.rs`.
- ADR 0019's "Code generator" section is refined: `cranelift-jit` is not used. Its macOS row is met since TASK-101.3.2 (`MAP_JIT`, see the table above).
- The JIT stays off by default, and spec section 1 ("no JIT" in the default build) still holds.

## Addendum: inline IC hits and mode `On`'s tiering rules (TASK-101.3.2)

### What compiles inline now

With a heap layout from the VM (`HeapLayout`), compiled code runs these hits itself and calls the helper (`Runtime::slow`, same pc) on any miss:

- named loads and stores, monomorphic up to the site's 4 entries: own data slots (inline and overflow), prototype data behind a shape compare plus `proto_epoch`, adding a property along a cached transition, and array `length`;
- dense-array element loads and stores, including the append at `length`.

Guards are compares of a shape id, the prototype id, the epoch and class tags. A failed guard runs the generic path in place, so nothing deoptimizes, and the frame holds nothing the interpreter's does not.

The heap is read through a base address that compiled code reloads after every call out, and it never keeps a heap address in a machine register across one (invariant I3 in `docs/design/jit-deopt-and-gc-maps.md`, section 1.1). Any helper may collect, and the birth-serial scheme holds as before. Under GC stress every back-edge in compiled code also stops at a safe point (`Runtime::safe_point_due`), and `heap_values_survive_collections_at_every_call_out_from_compiled_code` runs with a collection at every one.

### Tiering in mode `On`

Measured, compiling costs about 25 µs per bytecode instruction (Cranelift, with about 20 IR instructions per bytecode instruction once the IC paths are expanded), which is the cost of interpreting some 10,000 instructions. Entering compiled code and leaving it for a call costs more than the interpreter's dispatch of a call: a call round trip is about 30 ns, and a call out about 11 ns more than the interpreter's own step. So mode `On` compiles a block only when all of these hold:

- it has been offered 1000 times (`HOT_THRESHOLD`), and its static estimate is profitable then. The estimate now needs a loop, and costs named sites by their feedback: hits compiled code runs cost nothing, accessors count as calls;
- it has since taken `HOT_PER_INSTRUCTION` (200) loop back-edges per instruction. Only back-edges count here, and these offers are counted inline in the dispatch loop.

A compiled block is then judged as it runs, over a window that spans its runs (`JitState::judge`). It goes back to the interpreter for good (`JitStats::abandoned`) once, after 1024 back-edges or entries, its call-outs plus 20 per entry (`ENTRY_SHARE`) come to more than one per six instructions of the loop body per back-edge.

### Measured

Release build, i9-9900KF, Windows 11, binaries built from the same path. Each figure is the median over 5 alternating rounds of each process's median sample. Ratios are over the interpreter.

`agnijs-js-bench`:

| Workload | Feature off | `on` | `force` |
| --- | --- | --- | --- |
| loop-arith | 104.1 ms | 0.35 | 0.35 |
| loop-array-index | 94.6 ms | 0.81 | 0.81 |
| prop-monomorphic | 79.2 ms | 0.76 | 0.77 |
| prop-polymorphic | 120.1 ms | 0.71 | 0.72 |
| prop-write-alloc | 83.5 ms | 0.90 | 0.99 |
| closures | 64.9 ms | 0.96 | 1.33 |
| calls-recursive | 56.7 ms | 0.97 | 1.30 |
| method-calls | 118.7 ms | 0.97 | 1.25 |

All other workloads are within 1 to 3% in mode `On`. The exception is json-stringify, at 1.07: it compiles nothing, and the same binary with the JIT off is as slow, so the difference comes from code layout.

Gable's benches (`run-bench.mjs`, `on` and `force` over off in the same binary):

| Bench | `on` | `force` |
| --- | --- | --- |
| rxjs-operators | 1.012 | 1.38 |
| rxjs-higher-order | 1.006 | 1.27 |
| rxjs-subject-fanout | 0.996 | 1.30 |
| signal-churn | 1.011 | 1.26 |
| signal-create | 1.017 | 1.30 |
| template-mount | 0.999 | 1.28 |
| template-update | 0.991 | 1.37 |
| list-diff | 0.996 | 1.16 |
| ui-bundle-startup | 0.992 | 5.28 |

- **Where the JIT pays:** loops of arithmetic, property access and array elements, by 1.2 to 2.9 times.
- **Where it loses:** code dominated by calls, closures and short runs. Every call leaves compiled code and every return re-enters it. Gable's benches are all of that kind. When forced, they lose 16 to 38%, and startup loses five times over through compile time alone. Mode `On` now leaves such code to the interpreter, where it is within noise.
- **What would change that:** compiled calls, meaning a frame push and a return in machine code, so that a call no longer leaves compiled code. That is a follow-up task. Cutting compile time, for example by sharing the IC paths as out-of-line stubs, would also let mode `On` compile sooner.

## Addendum: calls without leaving compiled code (TASK-101.3.3)

The title's "leaves compiled code for every call" no longer holds for a call to a function that is already compiled. Everything else in the decision stands: there is still no speculation, no deoptimization and no GC map.

### Nested entry

A `Call` in compiled code goes out through its own entry (`Ctx::call`, which runs `Runtime::call`). The interpreter runs the call instruction as before: frame push, stack-overflow check and function-entry poll. Most of the time that push leaves exactly one new bytecode frame, and its block may already have machine code at the frame's pc. In that case the runtime enters that code from inside the call out (`JitRt::enter_callee`), at most 64 levels deep (`MAX_NEST`). On the callee's `Return` the runtime pops the frame (`do_return`), polls, and hands the caller `Resume::Next` with the return value. The caller then continues in its own machine code at the next instruction, without going back to `Vm::jit_run`.

This needs neither deoptimization nor GC maps, for the same reason as the rest of the tier:

- The caller has flushed its state to its interpreter frame before the call out (I1, I2).
- No heap address stays in a machine register across the call out (I3).

Any other way out of the callee (`Frame`, `Fault`, `Interp`, `Done`, or a nest deeper than the cap) unwinds every nested machine frame back to `Vm::jit_run`. The interpreter frames are already valid at that point, so exceptions unwind exactly as in the interpreter, through frames that compiled code left. When the debugger is attached (`debug_hot`), or the heap's attention word is set, the runtime takes the full `after` path instead of the direct return, so stepping, breakpoints and safe points behave as before.

### Lighter call outs

Instructions that touch neither the frame stack nor the heap layout go through `Ctx::fast` (`Runtime::fast`): `LdaGlobal` with its cache, `LdaEnv`, `StaEnv` and `LdaCallee`. A `Some` result promises that nothing cached changed, so the trampoline does not reload the register window, the heap view or the counters. `LdaThis` compiles inline. A frame's `this` is fixed for the frame's life, so `exec::enter` stores it in `Ctx` once. A `GetNamed` or `SetNamed` that machine code hands over has already missed its inline cache, so it goes straight to the generic step, which probes the cache only once.

### Measured

Measured on the same machine and in the same way as above (median of 5 alternating rounds), with ratios over the feature-off interpreter in the same binary. The before column is the TASK-101.3.2 table above.

`agnijs-js-bench`:

| Workload | `force` before | `force` now | `on` now |
| --- | --- | --- | --- |
| closures | 1.33 | 1.07 | 1.01 |
| calls-recursive | 1.30 | 1.07 | 1.00 |
| method-calls | 1.25 | 0.89 | 1.01 |
| loop-arith | 0.35 | 0.37 | 0.37 |
| prop-monomorphic | 0.77 | 0.59 | 0.58 |
| prop-polymorphic | 0.72 | 0.58 | 0.57 |
| prop-write-alloc | 0.99 | 1.07 | 1.00 |

Every other workload is within 2% in mode `On`, and regex-tokenize is 9% faster.

Gable's benches, `force` over off:

| Bench | Before | Now |
| --- | --- | --- |
| rxjs-operators | 1.38 | 1.27 |
| rxjs-higher-order | 1.27 | 1.18 |
| rxjs-subject-fanout | 1.30 | 1.11 |
| signal-churn | 1.26 | 1.14 |
| signal-create | 1.30 | 1.21 |
| template-mount | 1.28 | 1.21 |
| template-update | 1.37 | 1.26 |
| list-diff | 1.16 | 1.12 |

Mode `On` is within 1.5% of off on every one of them, from 0.986 to 1.010.

### What this did not reach, and why

Forced calls are now cheaper, but closures and calls-recursive are still about 7% slower than the interpreter, and Gable's benches 11 to 27% slower. None of the remaining cost is speculation, so no deoptimization or GC maps would remove it. It is per-call glue spread over several Rust layers:

- `enter_callee` and `exec::enter` set up a `Ctx` for each call, about 22% of the time on rxjs-operators.
- The call and return trampolines.
- The frame push and pop that the interpreter shares: `push_function_frame`, `do_return`, and the register clearing in `regs.resize`.

The interpreter does the same push and pop, but without the second layer around it. Winning here needs the frame push and the return in machine code, or a much lighter `Ctx` set up once per nest rather than once per call. The `Frame` representation (a `Vec` of frames holding `Rc`s) makes the first of these hard to do from Cranelift. For that reason mode `On` still compiles only blocks with a loop, and loop-free callees stay in the interpreter.

The prototype became a four-byte `OptObj` in TASK-101.3.2. That change also left four bytes of padding in `ObjectData`, which made the feature-off interpreter 6% slower on prop-write-alloc (bisected; same-path builds). The padding is now an explicit zeroed field, which brings it back to 0.98 of the pre-JIT build. promise-chain was noise.

## Addendum: deoptimization and GC maps (TASK-101.3.1)

The title's "never speculates" no longer holds. The tier now keeps JS registers in machine state across call outs and guesses that some of them hold numbers; both need the two pieces the decision said it did not: a description of the machine state at every call out (for the collector and for a debugger) and a way to leave for the interpreter when a guess fails. The design is `docs/design/jit-deopt-and-gc-maps.md`, which this addendum summarises. Everything else stands: Cranelift's code generator alone, one entry per block, no stack scanning (J6), no code patching (J9), and the interpreter frame as the only frame format (J1).

### Points: safe points, side exits and entry guards in one table

The code generator emits a `Points` table per block. A point is one of:

- a **safe point**, at every call out, with the pcs compiled code may continue at after it (`next`);
- a **side exit**, where a guard fails, naming the guarded register;
- an **entry guard**, at an entry pc, naming the register guarded there.

Each point lists the registers whose current value is not in the register file: `(register, Location)`, where the location is a slot in the compiled frame's spill area, holding a boxed `Value` (`Spill`). (As first built, a register held as an unboxed number was listed as its bits, `SpillNumber`, and boxed by the runtime. Since TASK-103.34 it is spilled as a value, under the tag it has: see below.) Before each call out machine code stores the point's id into `Ctx::point`; the spill area's address is in `Ctx::spill`, written once at entry. A `FrameHandle` (the `Ctx` pointer and the block's `Rc<Points>`) reads that state from Rust: the live heap values for the collector (`FrameHandle::live`) and the whole `FrameState` for a debugger or a deopt.

### Frames the collector finds without scanning the stack

`Runtime::frame_entered` and `frame_left` keep a list of the running compiled frames in the VM (`JitState::frames`, innermost last). The collector walks the list after the register file and traces each frame's listed values. A collection only ever happens inside a call out, where the point is set, so the state is always described. The birth-serial bound of nested runs (TASK-100.16) composes with this: it keeps Rust locals alive, the points keep machine state alive, and the contract test `a_nested_run_under_optimised_code_keeps_the_birth_serial_bound` runs both.

### Materialisation and side exits

Leaving compiled code anywhere but at a safe point's successor materialises the frame: the runtime reads the point's registers and writes them into the interpreter frame (`Runtime::materialize`, `Vm::jit_write_state`). That happens

- in the call-out trampoline, when the runtime continues at a pc the point does not list (a throw caught in the same frame, a debugger's step, a fault) or exits;
- in the deopt trampoline, when a guard fails: the exit's pc and accumulator are written too (`SideExit`) and the interpreter continues at the guarded instruction, which has not started;
- when a debugger attaches (`Vm::jit_materialize_all`), for every frame on the list, so what it sees is current. A frame materialised this way is not written again when its call out returns, so edits the debugger made are kept.

All of it is Rust reading through a raw pointer to a `Ctx` that lives on `exec::enter`'s stack; the only `unsafe` is still in `exec.rs`, and the handle's reads are plain enough to run under Miri (`exec::tests`). Fault and panic injection in the contract tests show a fault leaving the frame complete, and a panic in a call out unwinding through the machine frame and dropping the handle.

### What the code generator does with it

Up to eight registers that loops read or write inline are cached in Cranelift variables (tag and payload, or the unboxed number bits for a speculated one). At a call out, the registers the instruction reads are written back to the file; the other dirty ones are spilled and listed. After the call, every cached register is reloaded, because the runtime may have written any of them. At each entry a register speculated as a number is guarded only when it is live there (a liveness analysis over the cache); one that is written before read takes its payload as it is. A `Star` or `Mov` into a speculated register guards the stored tag.

Since TASK-103.34 two things about that are different. The liveness analysis has exception edges: an instruction inside a handler's range that can throw has the handler as a successor, so a local a `catch` block reads is live, and guarded, wherever a throw could carry it there. And a register taken without a guard keeps the tag word it was loaded with beside its payload until compiled code writes it, and is spilled, written back and copied with that tag. So the frame a fault, a side exit or a debugger materialises holds the register's value, never its payload bytes boxed as a number, which is what a handler saw before (`docs/jit-review-2026-11.md`). `docs/design/jit-deopt-and-gc-maps.md`, section 8, has the rule.

### The VM's guesses, and giving them up

When a block is compiled the VM guesses that the registers its loops do arithmetic on (`number_candidates`) that hold a number, or nothing yet, in the live frame will only ever hold numbers. ("Nothing yet" counts, since TASK-103.34, only for a register that is dead at the frame's pc by the same liveness analysis: written before anything, a handler included, reads it.) A guess that fails leaves through the side exit, the register is marked in the block's `Tier::demoted` and the block's code is dropped and compiled again without that guess the next time it is offered (its hotness is kept, so in mode `On` that is at once). After four failures a block stops guessing. The run that was in the old code holds its own `Rc` to it, so the replacement is safe at any point. `JitStats::speculated` and `deopts` count the guesses and the exits.

## Addendum: frame push and return in machine code (TASK-101.3.3, second part)

The first part of the task (the addendum "calls without leaving compiled code" above) entered a compiled callee from inside the call out, and ended with forced calls still slower than the interpreter: the cost was per-call glue in Rust, not speculation. After TASK-101.3.1 landed on top of it, that glue had grown again: at `1d172aa` the forced call workloads measured 1.27 (closures), 1.25 (calls-recursive) and 1.05 (method-calls) of the interpreter. This part removes the glue: a call of a compiled function is made by machine code. The title's "leaves compiled code for every call" is now wrong for most calls. The interpreter's frame stack stays the only one (ADR 0019 J1), and none of what follows speculates.

### A call made by machine code

- **The runtime describes its frame stack** (`CallLayout`), as it describes its heap (`HeapLayout`): where the frame vector and the register file keep their buffer, length and capacity, the size of a frame and the offsets of the fields a call sets, and where a function object keeps its code block, environment, realm, module and `this` mode. All of it is `offset_of!` or a difference of addresses, computed once in safe code; `None` when anything is not as compiled code reads it, and every call then goes through the runtime.
- **A callee table per VM** (`CalleeTable`, 256 buckets of 4) maps a code block to what a call of it needs: the machine code's address, the callee's inline-cache table and code info, its register count and parameter registers, its realm, and an *image* of the frame to push, which the VM built as a `Frame` in safe code. The runtime adds a callee the first time compiled code calls it through the runtime, if its compiled code keeps no register in machine state and its block does not keep its arguments. The table is shared by every call site, so a site that calls many functions (rxjs's operator callbacks) is served as well as one that calls one.
- **One call thunk** (`codegen::compile_thunk`) holds the sequence; a call site is the call's safe point and a call of the thunk with the instruction's operands. The thunk looks the callee's code block up in the table, checks that nothing stands in the way (no poll due, no safe point wanted, the native stack above the VM's floor, room in both vectors, the callee's inline caches of the current shape epoch, a `this` the callee takes as it is or replaces with its realm's global object), writes the caller's frame back, pushes the callee's frame by copying the image and setting the fields that differ per call, clears the callee's registers and copies the arguments it takes, and calls the callee's machine code with a context built in the thunk's own stack frame. On the return it pops the frame, counts the poll after a call returns, and gives the caller the value. Any check that fails makes the call through `Runtime::call` from the same state; a callee that leaves compiled code instead of returning makes each machine frame below it leave too, its frame materialised first.
- **The context is split** into what a run shares (`Shared`: the runtime, its entries, the interrupt counter, the heap view) and what a frame owns (`Ctx`: its register window, accumulator, `this`, inline-cache table and point). The VM's side of the runtime is per run, and finds its frame at the top of the frame stack.
- **Nothing new for the collector or a debugger.** A function called directly keeps nothing in machine state, so its interpreter frame is complete at every call out and it needs no frame handle. A function that does keep registers in machine state (one with a loop on numbers) is never called directly; the nested entry of the first part still runs it. The contract machine code is trusted with is two invariants beside I1 to I3, stated in `docs/design/jit-deopt-and-gc-maps.md`, section 9: a frame machine code pushes is a copy of one the runtime built, with typed fields set (I4), and machine code neither grows nor frees a vector or the last reference to a code block (I5).
- **The native stack bound is in bytes.** Machine frames nest on the native stack now, and their size depends on the function, so the bound of 64 levels became a floor for the stack pointer, `JIT_STACK` (96 KB) below where compiled code was first entered, or the limit of the host's stack budget (`Engine::set_stack_budget`, TASK-103.27) when that is nearer. Past it a call leaves compiled code as it always could.

### What else stays in compiled code

- **A callee without compiled code runs in the interpreter from inside the call out** (`interpret_callee`), as a run loop that ends when its frame returns. Leaving compiled code for it unwound every machine frame on the native stack, each to be entered again from memory as the calls returned: a compiled function that called `new` on a derived class paid that at every call (Gable's signal-create entered compiled code 60,000 times an iteration).
- **A base class's constructor compiles** like any function called with `new`. Derived constructors, generators and async functions still do not.
- **Environment slots** (`LdaEnv`, `StaEnv`) are read and written by machine code through `EnvLayout`, from the running frame's innermost environment outwards. With the `jit` feature `Environment` is `#[repr(u8)]`.
- **Global bindings** (`LdaGlobal`) are read at the address a `GlobalSite` holds, while the current realm and the heap's *global stamp* are the site's. The stamp moves when a key is added to or removed from any object in dictionary mode (the global objects are) and when a global lexical is declared, which is when such an address can move.
- **Equality** (`==`, `!=`, `===`, `!==`, as values and as fused branches) is decided inline for every pair that needs neither the heap nor a conversion. Before, only two numbers were.
- **Code that keeps nothing in machine state skips the successor check** after a call out (it has nothing to materialise).

### Tiering in mode `On`

Calls made by machine code change what is worth compiling, so the rules of the TASK-101.3.2 addendum gained three.

- **A function without a loop can be worth compiling**: the static estimate (`profitable_for`) no longer needs a loop when calls can be made directly, counts a `Call` as one call out, not two, and counts environment and global loads as inline.
- **But only where machine code will call it.** Entered from the interpreter, a small function costs more compiled than interpreted, and compiling costs what interpreting thousands of instructions does. So a hot function without a loop is compiled when the frame calling it is its own block's (recursion) or a compiled block's. Otherwise it waits, uncounted (`Tier::DEMAND`), and is compiled once compiled code has called it 50 times per instruction.
- **Each block's runs are judged by what they cost against the work they do** (`jit_judge_run`): the entry from the tier loop, its call outs and the callees it ran in the interpreter, against the loop back-edges it took and the calls it made directly. A block that costs more than its work is worth is not entered from the interpreter again; its code is kept for compiled callers (`Tier::DIRECT`). The judgement is per block, where the older window was shared and reset whenever another block was judged.

### Measured

Same machine, method and units as the tables above: i9-9900KF, Windows 11, release builds from one path, the median over alternating rounds of each process's median sample (5 iterations after 1 warm-up), as ratios over the same binary with the JIT off. "Before" is `1d172aa`, the commit this work started from.

`agnijs-js-bench` (5 rounds):

| Workload | `force` before | `force` now | `on` before | `on` now |
| --- | --- | --- | --- | --- |
| closures | 1.26 | 0.69 | 1.00 | 0.78 |
| calls-recursive | 1.23 | 0.38 | 0.97 | 0.37 |
| method-calls | 1.04 | 0.77 | 0.99 | 1.02 |
| loop-arith | 0.32 | 0.32 | 0.31 | 0.32 |
| loop-array-index | 0.56 | 0.26 | 0.54 | 0.26 |
| prop-monomorphic | 0.59 | 0.31 | 0.58 | 0.30 |
| prop-polymorphic | 0.53 | 0.27 | 0.52 | 0.28 |
| prop-write-alloc | 1.24 | 1.33 | 0.99 | 0.99 |
| string-concat | 1.26 | 1.30 | 1.02 | 0.99 |
| array-methods | 1.14 | 1.21 | 1.00 | 0.98 |
| map-set | 1.05 | 1.11 | 0.97 | 1.00 |
| regex | 1.03 | 1.03 | 0.99 | 1.05 |

The other workloads are within 3% of the interpreter in mode `On`, before and now (the two sub-millisecond start-up pairs move by more between runs).

Gable's benches (5 rounds, the harness of Gable `7cd58eb` built against this tree):

| Bench | `force` before | `force` now | `on` before | `on` now |
| --- | --- | --- | --- | --- |
| rxjs-operators | 1.42 | 0.78 | 1.00 | 0.99 |
| rxjs-higher-order | 1.33 | 1.02 | 1.01 | 1.02 |
| rxjs-subject-fanout | 1.25 | 0.74 | 1.00 | 0.74 |
| signal-churn | 1.29 | 1.03 | 1.00 | 1.01 |
| signal-create | 1.33 | 1.00 | 1.02 | 0.99 |
| template-mount | 1.32 | 1.19 | 1.00 | 1.02 |
| template-update | 1.39 | 1.15 | 1.00 | 1.01 |
| list-diff | 1.21 | 1.08 | 1.00 | 1.03 |
| ui-bundle-startup | 10.2 | 10.9 | 1.00 | 1.00 |
| geomean | 1.65 | 1.29 | 1.00 | 0.97 |

- **Forced calls now beat the interpreter** on the three call workloads, by 1.3 to 2.7 times, and on two of Gable's five rxjs and signal benches. The other three (rxjs-higher-order, signal-churn, signal-create) are at the interpreter's speed, not under it: a run of 10 rounds put them at 1.03, 1.03 and 1.02, with the same binary against itself at 1.00.
- **Mode `On` gains where calls are direct**: calls-recursive (20.6 ms, where QuickJS-NG takes 33.7 ms), closures and rxjs-subject-fanout. The loop and property workloads that read globals halved again, because the global loads in their loops are inline now.
- **Where forced code lost a little**: workloads that are mostly call outs (prop-write-alloc, string-concat, array-methods, map-set) are 4 to 9 points slower forced than before. A call out finds its frame at the top of the frame stack now instead of being handed it, and a call of a native goes through the thunk before it reaches the runtime. Mode `On` does not compile such code.
- **regex is 5% slower in mode `On`.** Its loop calls natives only; the estimate takes a `Call` for a call of a compiled function, so the loop is compiled (49 ms) and then given up, and the iterations after that, all interpreted, stay about 5% slower than in a process that never compiled it. Why they stay slower is not established (the allocator's state after a large compilation is the guess; the workload spends a third of its time allocating). Counting a `Call` in a loop as two again removes the loss and also the gains on closures and rxjs-subject-fanout, so it was not done; telling a call of a native from a call of a function needs call-target feedback, which the bytecode does not have yet.
- **The interpreter without the feature is unchanged**: 25 workloads, head over `1d172aa`, built from one path, geomean 1.002 (the same binary against itself: 1.003).

### What this did not reach

- **Calls that are not a `Call` of a plain function.** `new`, getters and setters, and calls through `Function.prototype.call` and `apply` still push their frame in the runtime and enter the callee from inside the call out. method-calls is half getters, which is why mode `On` gains nothing there yet.
- **Megamorphic property access.** Gable's signal-churn spends 78% of its call outs on `GetNamed` at sites with more than four shapes, which the interpreter also resolves the long way; a shared cache keyed by shape and key would serve both tiers.
- **`typeof x === "..."`** is three call outs (`Typeof`, a string constant, a string comparison); a fused instruction in the bytecode would serve both tiers.
- **Mode `On` on call-heavy code that is not a loop or a recursion.** Forced, rxjs-operators runs at 0.78 of the interpreter; tiered it stays at 0.99, because the functions that would call each other directly are entered from interpreted callers and are (rightly, one at a time) not compiled. Compiling a group of functions that call each other needs call-target feedback too.
- **Compile time.** Forced, a 100 KB script still takes seconds to compile (compile-source: 8.4 s before this work, 8.9 s after), most of it in a few very large blocks.

## Addendum: cached registers across call outs, and what counts as a run's work (TASK-103.56, TASK-100.36)

Two follow-ups to the addenda above, measured in `docs/js-engine-performance-findings.md` (section "Tiered JIT follow-up, 2026-11-14"). Nothing in the decision changes.

### Cached registers are read back after a call out

"After the call, every cached register is reloaded, because the runtime may have written any of them" was true of a generic instruction only. After an instruction whose register access is known, compiled code went on with the values it held before the call, so every cached register was live across every call out of the block. A call preserves seven general registers on Windows x64 and five on other x64 systems; up to sixteen words of cached registers and the two context pointers wanted them, and the register allocator put what did not fit on the stack, whether a loop read it or not. After TASK-103.34 that was the shared-state pointer in prop-monomorphic's inner loop, 5% of the workload.

Now the cached registers are read back after every call out: from the register file when the call's point wrote the register back for the instruction to read, or compiled code never writes it; from its spill slot otherwise. Nothing writes either during the call (a known instruction writes no register, and a debugger that attaches makes the frame leave compiled code). The point, what it lists and what the runtime materialises are as they were, so the rule of TASK-103.34 holds unchanged: a register is spilled, written back and copied as a value, never boxed from bits. A speculated register's tag word is not read back. It is a constant wherever the register has been guarded or written, and reading it would make it a value to merge after every call out.

Call outs are on cold paths, so loops lose nothing: prop-polymorphic and loop-array-index gained 6 to 7%, prop-monomorphic is back within layout noise of the commit before TASK-103.34 (1.024). Forced code that is mostly call outs pays for the reads (prop-write-alloc 1.03, string-concat 1.02 to 1.05).

The two context pointers still cross every call, and the allocator still keeps the shared-state pointer on the stack in some loops. Reading them back too made the same loops slower, so they are left.

### A run's work is what compiled code did

Mode `On` judges a block by the work its runs do without leaving, read off the interrupt counter (`JitStats::back_edges`). Since TASK-103.28 natives advance that counter as well, once per step over their input, and a run nested under a native advances it by its own work. Neither is the calling run's: counted, a loop that only calls `exec` and `replace` looked seventy times as busy as it was and was never given up. Whatever the counter advances by while the runtime runs an instruction for compiled code is now left out (`Vm::jit_uncounted`), as a callee's own run already was.

### Which registers are guessed to be numbers

A register the block also uses as the object of a property access or as a callee is not a candidate (`number_candidates`). The bytecode compiler reuses its temporaries, so such a register held a number at the back-edge where the block was compiled and an object two instructions later: the guess failed at once, and nearly every loop over objects was compiled twice.

### Looking at the code

`AGNIJS_JIT_DUMP=<directory>` writes the machine code of every block compiled as `<n>-<function>.bin` and names each file on stderr. The bytes carry no relocations: `llvm-objcopy -I binary -O elf64-x86-64 --rename-section .data=.text,code` and `llvm-objdump -d` read them. `AGNIJS_JIT_STATS=1` gives the counts to read beside it.
