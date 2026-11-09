# 19. Later tiers share the interpreter's frames, and the interpreter, bytecode, object model and collector keep the constraints that make them possible

Date: 2026-11-09

## Status

Accepted

## Context

ADR 0011 keeps v1 interpreter-only and asks for "room for a later tier". TASK-101 plans the later tiers: heap and realm snapshots (TASK-101.2), a baseline JIT (TASK-101.3), an optimizing tier if the baseline justifies it (TASK-101.4), and research into native ahead-of-time compilation (TASK-101.5). Before any of that starts, six interpreter performance tickets rewrite the parts a JIT depends on:

- the dispatch loop (TASK-100.17);
- the bytecode shape (TASK-100.18);
- calls and native re-entry (TASK-100.19);
- inline caches (TASK-100.20);
- object, function and string layout (TASK-100.21);
- collection under native frames (TASK-100.16).

Each of those can easily make a choice that blocks a JIT, for example interpreter state that lives only in Rust locals, superinstructions with two observable effects, feedback that machine code cannot read, or a rooting scheme that needs the Rust stack scanned. This ADR fixes the constraints those tickets and every later engine change must keep. It also records how tiers are organised, how they deoptimize, what is optional, and where a JIT may run at all.

The current engine (checked at `59fb104`, 2026-11-09) is already close:

- Frames are `Frame` records over one shared register file (`Vm::regs`).
- `pc` is an instruction index into a fixed-size `Instr` enum.
- Exceptions are `Result`s routed through a per-block handler table.
- Inline caches are a per-`CodeBlock` side table of plain ids.
- Shapes are freed only when nothing names them: since TASK-100.22 the collector traces shapes from live objects and from the inline caches of live blocks, and clears untraced caches before reuse.
- The heap is precise, non-moving and index-based, and collects only at safe points.
- The engine interface (`agnijs-engine-api`) exposes no interpreter type, only opaque bytecode bytes.
- No engine crate uses `unsafe`; the only uses are in the QuickJS-NG FFI adapter and the benchmark's process-memory probe. Since TASK-100.32 every engine crate (`agnijs-js-parser`, `-compiler`, `-vm`, `-regex`, `-intl`, `-debugger`, `-cdp` and `agnijs-engine-api`) enforces this with `#![forbid(unsafe_code)]` at the crate root; `agnijs-engine-quickjs` (FFI into QuickJS-NG) and `agnijs-js-bench` (the process-memory probe calls `K32GetProcessMemoryInfo`/`getrusage`) are the two crates that keep it.
The gaps are listed in the spec, section 14.

## Decision

### Tiering

- **The interpreter is the reference tier, the default and the fallback on every platform.** Every other tier is optional, and every test262 and embedder test must pass with it disabled (ADR 0011).
- **The baseline JIT uses the interpreter's frame layout** (the approach of V8's Sparkplug). A baseline frame is an ordinary `Frame` over the same register window, with the same register numbering, `acc` and `pc`. Baseline code makes no speculation the interpreter does not. It calls the same Rust helpers for generic cases and inlines only the inline-cache hit paths and simple arithmetic on numbers. As a result:
  - leaving baseline code means continuing in the interpreter at the frame's `pc`;
  - on-stack replacement at a loop back-edge means jumping into baseline code at the same `pc`;
  - the debugger, stack traces, the collector and generator suspension see one frame format.
- **Tier-up is driven by per-`CodeBlock` hotness counters.** The counters are bumped at function entry and at loop back-edges, the same points that poll the attention word (constraint J7). They are a side table, like the inline caches, and are compiled in only with the `jit` feature.
- **An optimizing tier (TASK-101.4), if built, deoptimizes to interpreter frames.** Each guard carries a frame-state map: the bytecode `pc`, the location of `acc`, and the location of each live register (a machine register, a spill slot or a constant). A failed guard (eager deoptimization) rebuilds the interpreter frame and continues at that `pc`. When code is invalidated under a frame that is still running, for example because a prototype it assumed changed, the frame is marked. On return into it, the frame is rebuilt at the call's `pc + 1`, with the call's result in `acc` (lazy deoptimization). Deoptimization targets are always instruction boundaries. No tier invents a frame format the interpreter cannot resume.
- **Machine code is never serialised.** The bytecode container (ADR 0010) stays tier-independent. JIT code is an in-memory cache owned by its `CodeBlock`, and a snapshot (TASK-101.2) never contains it. AOT output (TASK-101.5) is a separate artefact with its own format tag, bound to one engine build like bytecode.

### Constraints

These hold for every engine change from now on. The six tickets above carry a JIT-readiness note that names the ones that apply to them.

- **J1. One frame stack, one register file.** Every tier keeps its frames as `Frame` records in `Vm::frames`, over windows of `Vm::regs`. Code may keep `pc`, `acc` and the register window in machine or Rust locals between observation points. It must write them back before anything that can observe a frame:
  - a call into the runtime or into JavaScript;
  - a throw;
  - a safe point;
  - a debugger stop;
  - a generator suspension.

  The register file may reallocate when a frame is pushed, so code addresses registers by base index and reloads any cached window pointer after a call.
- **J2. `pc` is an instruction index, and it is the one bytecode position.** Handlers, inline-cache sites, debugger statement starts and breakpoints, source offsets, deoptimization targets and on-stack replacement entries are all keyed by it. An encoding change (TASK-100.17 threaded code, TASK-100.18 new forms) keeps an O(1) mapping between `pc` and instruction. Instructions stay decodable without executing them, so a compiler can walk a block linearly.
- **J3. Every instruction's generic semantics is a callable Rust function, and each instruction is atomic for deoptimization.** The dispatch loop may inline fast paths, but the full behaviour of each opcode must also live in a function on `Vm` that takes operand values and returns `JsResult`, which a JIT can call. Within one instruction, every observable effect (`valueOf`, `toString`, getters, setters, proxies, calls) happens inside that function. The only work left after the last effect is writing one result into `acc` or one register, or taking one branch decision. So a frame can always resume at `pc` (if the instruction faulted before any effect) or at `pc + 1`. Superinstructions follow the same rule. For example, a fused compare-and-branch performs both `ToPrimitive` conversions, then decides the branch, and writes nothing else.
- **J4. Feedback is plain data in per-`CodeBlock` side tables, indexed by a compiler-allocated slot.**
  - Inline caches, and any later call-target or arithmetic-type feedback, never widen an instruction and are never serialised (they load empty).
  - An entry holds only ids and small integers: shape ids, holder identity and shape, slot index, and a kind (own data, prototype data, accessor, array length, and so on). Machine code can read it at a fixed offset without calling into the VM.
  - Entry states are explicit: empty, monomorphic, polymorphic with at most N entries, and megamorphic. Megamorphic sticks until the block is recompiled, so a JIT can trust it.
  - Feedback belongs to one heap. If a `CodeBlock` is ever shared between heaps (TASK-103.11), its feedback moves to a per-heap table first.
- **J5. Shapes are the guard currency, and the hot object fields have a defined layout.**
  - Shapes are never freed, or are freed only when no feedback entry and no JIT code names them.
  - A shape fixes each slot's key and attributes, so the kind of a property (data or accessor, writable or not) can be decided from the shape at fill time (ADR 0012).
  - Before TASK-101.3 starts, `Value` and the inline-cache hit path get a defined layout: `#[repr(C, u8)]` or a NaN-boxed representation for `Value`, and a `#[repr(C)]` object header with the shape id, the dictionary-mode flag, the class tag the cache tests and the slot storage, reachable through `offset_of!` constants. Slot storage stays a contiguous run of values: inline slots plus an overflow buffer is fine, as long as a fixed rule says which slot index is inline.
  - Arenas may move when they grow, so no raw pointer into a heap object outlives a runtime call.
- **J6. The collector is precise and non-moving, roots are explicit, and nothing scans the native stack.**
  - Safe points are runtime calls and attention polls. At a safe point, every live value is in the register file, a frame, or the scoped root stack. Tiers therefore need neither stack maps nor conservative scanning.
  - Natives and JIT runtime helpers root their temporaries through the same mechanism, the one TASK-100.16 introduces. That mechanism must not depend on scanning the Rust stack.
  - Heap ids that machine code embeds as constants are rooted from the code object, or re-validated on use by identity and shape the way the inline cache does (ADR 0012).
  - There is no write barrier today. If an incremental or generational collector adds one, it is a single `Heap` function that every store of a value into the heap goes through, with an inline fast check a JIT can emit.
  - A moving collector reopens this ADR, because it needs either stack maps or handle indirection in machine code.
- **J7. One attention word.** Interrupt requests, the memory limit, GC requests, GC stress mode and the debugger all funnel into one word at a fixed field of `Vm` (TASK-100.17). Every tier polls it at function entry, at loop back-edges and after a call returns. Each poll is a load, a compare and a branch, and the slow path is shared Rust code. As built by TASK-100.17 the word is `Heap::attention` (`ATTENTION_GC`, `ATTENTION_DEBUG`); `Heap` is a by-value field of `Vm`, so the word sits at a fixed offset from the `Vm` pointer, and it lives in the heap because the allocator is its most frequent writer.
- **J8. One way into JavaScript, one way to push a frame, faults as values.**
  - Natives, host entry points, jobs and `call`/`apply` reach JavaScript through one function, which is where tier selection lives. JavaScript-to-JavaScript calls push frames through one path, and arguments are a slice of the caller's register window (TASK-100.19).
  - A fault is a returned `Fault`. Handler lookup runs on the interpreter frame at the written-back `pc`. Machine code returns a fault to the runtime and never unwinds the Rust stack.
  - Generators and async functions suspend into the interpreter's `SavedFrame` form and resume in the interpreter. A tier may later compile resume points, and these are instruction indices.
- **J9. The debugger and hot update force the interpreter.**
  - While a debug client is attached, the attention word sends every frame back to the interpreter at its next poll, and no code is tiered up.
  - The debugger reads and writes only interpreter-format frames. With J1 these are the only frames, so evaluating in a paused frame and writing back register locals (ADR 0017) keeps working.
  - Module reloading creates new `CodeBlock`s, and JIT code dies with the block that owns it. No tier patches code in place for hot update.
- **J10. The embedding surface and the build do not lock the engine to interpretation, and `unsafe` stays confined.**
  - `agnijs-engine-api` names no interpreter type, and it never will. Choosing a tier is an engine option: off by default, plus a forced-JIT mode for tests. `Engine::info` reports the tier in use.
  - The JIT is a cargo feature, `jit`, on `agnijs-js-vm`, off by default. The feature pulls in a separate crate, `agnijs-js-jit`, which is the only engine crate allowed `unsafe` (executable memory, entering machine code). Embedder-facing FFI crates such as the QuickJS-NG adapter and the C API (TASK-103.3) keep their own `unsafe`.
  - The runtime helpers that machine code calls are `extern "C"` functions with non-generic signatures, defined in one module. Together they are the contract shared by the JIT and the AOT work, and that contract is versioned with the engine build.
  - Engine crates stay embedder-agnostic (ADR 0016).
- **J11. A realm's heap can be written out as data (for snapshots).**
  - Everything reachable from a realm is heap data, a registered id or host data with a serialisation hook:
    - native functions are identified by a stable index into a registry built at realm creation, not by a Rust function pointer;
    - state shared between native closures lives in the heap, not in an `Rc<Cell<_>>`;
    - a host object without a serialisation hook makes snapshotting fail with an error, not silently.
  - Code blocks are written once each through the existing bytecode serialiser (`agnijs-js-compiler/src/serialize.rs`).

### Code generator

The baseline JIT uses **Cranelift** (`cranelift-codegen`, `cranelift-frontend`, `cranelift-jit`):

- It is pure Rust.
- One IR covers x86-64 and AArch64, the two architectures that matter on Windows, Linux, macOS and Android.
- It has a verifier.
- It can emit object files (`cranelift-object`), which TASK-101.5 needs for AOT. So one code generator serves the baseline JIT, the AOT route to iOS and an optimizing tier if one is built.

A hand-rolled template assembler, such as `dynasm-rs` per architecture, compiles faster. But it doubles the hand-written machine code and the `unsafe`, and it cannot help AOT.

The cost is Cranelift's compile latency and dependency weight. Both are contained: only hot functions compile, and the dependency is behind the `jit` feature. TASK-101.3 measures compile time per function on the benchmark workloads before it commits. If compilation takes more than about a fifth of the measured gain on the Gable set, it records that and reopens this choice.

### Platforms

| Target | Baseline JIT | Story |
| --- | --- | --- |
| Windows, Linux (x86-64, AArch64) | Yes, with the `jit` feature | Executable memory is mapped write-then-execute, never writable and executable at once. If a process forbids dynamic code (Windows Arbitrary Code Guard, a hardened Linux policy), the engine falls back to the interpreter. |
| macOS | Yes, if the embedder opts in | The embedder's app must carry the hardened-runtime entitlement `com.apple.security.cs.allow-jit`. The engine uses `MAP_JIT` and per-thread write protection on Apple Silicon. Without the entitlement, the mapping fails and the engine falls back to the interpreter. The default build needs no entitlement, as spec section 1 requires. |
| iOS, iPadOS | Never | Third-party apps cannot map executable memory. These targets get the interpreter, snapshots (TASK-101.2) and, if TASK-101.5 recommends it, AOT code linked into the app at build time. The `jit` feature does not compile for these targets. |
| Android | Yes, if the probe succeeds | Ordinary apps can JIT, but locked-down builds and per-app policies (hardened distributions, enterprise profiles) refuse executable mappings. The probe at engine creation decides, and the interpreter is the fallback. |
| WASM (wasm32) | Never | A module cannot add machine code to itself. Generating and instantiating new WASM modules through the host is out of scope. The `jit` feature does not compile for this target. |

The probe runs once when the engine is created. It maps a page, makes it executable and runs a tiny function. If any step fails, the engine silently uses the interpreter, and `Engine::info` reports it. Unsupported architectures behave the same way.

## Consequences

- TASK-100.16 to TASK-100.21 have named constraints to check against, and their JIT-readiness notes point here. A choice that breaks a constraint needs an addendum to this ADR, not a quiet workaround.
- The baseline JIT is cheap to make correct. With one frame format there is no deoptimization metadata, on-stack replacement is just a jump, and the interpreter remains the oracle: a differential fuzz test (TASK-101.3) compares the two on the same frames.
- The gain is bounded. A baseline that keeps the interpreter's frames removes dispatch and decoding but keeps every value in memory. Larger gains need TASK-101.4 and its frame-state maps.
- Layout work becomes a JIT prerequisite: a defined `Value` layout and an object header with known offsets (J5). It is cheapest to do during TASK-100.21's object rewrite.
- Spec section 1's goals ("no JIT", "no JIT entitlement on macOS") describe the default build and stay true. The spec's new section 14 lists these constraints and the current gaps.
- ADR 0011 still holds for v1. This ADR is what its "room for a later tier" means in concrete terms.
