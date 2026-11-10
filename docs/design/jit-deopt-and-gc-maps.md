# JIT deoptimization and GC maps: the hand-off specification

Status: specification for reserved work (TASK-101.3 hand-off). Nothing here is implemented. The stub interfaces are in `crates/agnijs-js-jit/src/handoff.rs`, and the ignored tests there name the contracts to meet.

Read first:

- ADR 0019, constraints J1 to J10;
- ADR 0020, the baseline tier and the reasons it needs neither piece;
- `crates/agnijs-js-vm/src/gc.rs`, the module docs on safe points and the retention bound.

## 1. Why the baseline has neither, and what changes that

The baseline tier, as built, keeps two invariants:

- **I1. No value outside the frame across a call out.** At every call from machine code into Rust (`Runtime::slow`, `Runtime::poll`, `Exit`), the accumulator has been flushed to the frame. The registers were never anywhere but `Vm::regs`. The only machine-held values across a call are the context pointer and immediates: no heap ids and no derived pointers. `Runtime::arith` is pure and sees no heap.
- **I2. No guard without a generic path.** Every inline fast path is guarded, and its failure branch calls `Runtime::slow` for the same instruction at the same pc, from a state that is already an interpreter frame. Compiled code can always continue after it, at the pc the runtime returns.

So the baseline needs no frame-state metadata (I2 makes every point a valid interpreter state) and no GC maps (I1 makes the frame stack the complete root set).

Each of the following optimisations breaks one of the invariants, and must not land until the matching piece below exists:

| Optimisation | Breaks | Needs |
| --- | --- | --- |
| JS registers or `acc` kept in machine registers across a call out | I1 | GC maps (section 4) |
| Unboxed numbers kept across instructions | I1 (once boxed values mix) and I2 | both |
| Inline IC hit paths reading the object arena directly | I2 if a shape miss jumps anywhere but `slow` at the same pc. With a miss path to `slow`, this stays baseline-safe. | deopt only if the miss path is dropped |
| Direct calls between compiled functions (machine frames on the native stack) | I1, and the frame stack model | both, plus section 6 |
| Inlining, or speculation on call targets or types without a generic fallback | I2 | deopt (section 3) |

Inline IC hit paths with a full miss path need neither piece. They are implemented (TASK-101.3.2, below).

### 1.1 Inline IC hits in the baseline (TASK-101.3.2)

Compiled code reads the object arena directly for named loads and stores that hit their inline cache (monomorphic up to the 4-way polymorphic form, own data and prototype data), dense array element reads and writes, and array `length`. Every guard is the interpreter's own: the receiver's shape against the cache entry's, and for a prototype hit also the holder's shape and `proto_epoch` against the entry's. Any miss calls `Runtime::slow` for the same instruction at the same pc (I2), so nothing is speculated and no deoptimization exists.

The heap view adds a third invariant, which keeps I1 true for addresses as well as values:

- **I3. No heap address across a call out.** The arena base (`Ctx::objects`) and the prototype epoch (`Ctx::proto_epoch`) are the runtime's (`Runtime::heap`) on entry and are refreshed by the trampoline after every call out that continues compiled code. The code generator loads them from the `Ctx` in each instruction that needs them and never keeps them, or any address or value derived from them, in a machine register across a call. Any call out may allocate (the arena may reallocate and move), collect, or change a prototype; the next instruction sees the new view.

Compiled code never changes an object's structure. It only stores values into data slots and dense elements its guards checked, so the shape tables, the IC entries and the collector's invariants are the interpreter's. A store writes a `Value` and needs no write barrier, because the collector is non-incremental. The layout compiled code assumes (`HeapLayout`) is computed at start-up from the VM's own types by address differences, and code is compiled without heap access when any check fails.

GC attention is serviced inside compiled code: every call out that continues it checks the attention word and collects there (`JitRt::attention`), with the frame written back. Under GC stress (`Runtime::safe_point_due`) every back-edge also calls `Runtime::poll`, so compiled loops collect at each loop head as the interpreter does. The test `heap_values_survive_collections_at_every_call_out_from_compiled_code` (in `crates/agnijs-js-vm/tests/jit_inline_cache.rs`) runs inline loads and stores around allocating calls under `heap.stress` with a collection at every safe point. It checks the result against the interpreter's.

Nothing here touches the birth-serial bound (section 6): compiled code still never recurses on the native stack.

## 2. Frame layout

There is one frame format (ADR 0019 J1): `Frame` in `Vm::frames`, over a window `Vm::regs[base..base + n]`, with `pc` and `acc`. Optimised code adds a *compiled frame* that shadows the interpreter frame. It does not replace it.

- **The interpreter `Frame` always exists.** It is pushed by the normal call path before compiled code is entered. Its `code`, `base`, `this`, environment and exit fields are authoritative at all times. Only `pc`, `acc` and some register slots may be stale while machine code runs.
- **The compiled frame is the machine stack frame of the Cranelift function.** Its layout:
  - the return address and saved callee-saved registers (Cranelift's ABI frame);
  - the spill area (`Location::Spill(offset)`), at fixed offsets from the frame's stack pointer and described per safe point;
  - no JS-visible layout of its own. Nothing outside the JIT reads it except through a `GcMap` or a `FrameState`.
- **`Ctx` stays the boundary.** It is `#[repr(C)]`, with the fields `regs`, `rt`, `slow`, `poll`, `arith`, `pc`, `counter`, `attend`, `proto_epoch`, `objects`, `ic`, `acc`, at `Offsets`. Fields machine code reads go before `acc` and get `Offsets` entries; the fields after `acc` are Rust's alone. The deopt entry point and the safe-point return-address register become new helper pointers in this struct, keeping the non-generic `extern "C"` rule (J10).
- **Stale register slots are allowed only between safe points.** At a safe point (section 5), each register slot is either current in `Vm::regs` or listed in the safe point's `live` set with its location. A slot holding a stale *heap* value while the current value is elsewhere must be listed. Otherwise a collection would trace the stale value, which is harmless, but a later deopt would write back the wrong one.

## 3. Deoptimization: the state at a side exit

A side exit is a guard failure, an invalidation or a debugger request that leaves optimised code for the interpreter at an instruction boundary.

- **Target.** Always the start of an instruction, `FrameState::pc`, which has not begun (J3). An exit never resumes in the middle of an instruction. Speculation is therefore checked *before* the instruction's first observable effect. Since every effect is inside one generic function (J3), the guard goes before the call to it.
- **What is materialised.** `FrameState`:
  - `pc`: the instruction to run next.
  - `acc`: its `Location`.
  - `regs`: `(register, Location)` for every JS register whose current value is not already in `Vm::regs`. Every register not listed must be current in the register file.
  - `raw`: the machine locations of unboxed numbers. Boxing a number allocates nothing (`Value::Number`), so materialisation never collects. This is required: a deopt must not reach a safe point while the state is half written.
  - Only the top frame is materialised. Inlined frames, if inlining is ever done, need one `FrameState` per inlined call, outermost first, each with its own `Frame` to push. That is out of scope until there is inlining, and the shape of `FrameState` may grow a `parent`.
- **Protocol.** The side exit calls the deopt helper with the exit's index. The helper looks up the `FrameState`, captures the machine locations (`read`), and calls `Deoptimizer::materialize`. That writes `Frame::pc`, `Frame::acc` and the listed register slots. The helper then returns `Exit::Interp`. `Vm::jit_run` already handles `Exit::Interp` by reloading the interpreter's locals and continuing there.
- **Reasons** (`DeoptReason`):
  - `Guard`: a type or shape check failed.
  - `Invalidated`: an assumption was broken from outside, for example a prototype epoch moved or a global was redefined. Eager invalidation marks the block with `Deoptimizer::invalidate(block_id)`. Frames still running it deopt lazily, at their next return into it: the return continues at the call's `pc + 1` with the result in `acc` (ADR 0019).
  - `Debugger`: J9. Attaching sets the attention word (`ATTENTION_DEBUG`). Every compiled frame deopts at its next poll, and no block tiers up while a client is attached. This is the `attaching_a_debugger_deoptimizes_running_frames` test.
- **Invalidation storage.** The `Tier` side table per `CodeBlock` (`compiler::bytecode::Tier`) holds the state (`COMPILED` or `NEVER`) and the code. Invalidation sets the state back to "not compiled" and drops the `Rc` once no frame is running it. Frames hold their own `Rc`, as `jit_run` does with `code.clone()`. Code is never patched in place (J9).

## 4. GC maps: how a collection under JIT frames finds roots

The collector is precise and non-moving, and it never scans the native stack (J6). Its roots are `Vm::regs`, `Vm::frames`, the scoped root stack and the retention bound.

- **Where collections happen.** Only inside Rust code called from machine code (section 5), never asynchronously. A collection always has a machine-code caller blocked at a known return address.
- **The map.** One `GcMap` per compiled function: a list of `SafePoint { return_offset, pc, live }`, sorted by `return_offset`. `live` lists every location (machine register, spill slot) that may hold a heap value (a `Value` with an object, string, symbol, bigint or environment payload) at that call.
- **Finding the frames.** Do *not* walk the native stack. Keep an explicit chain instead:
  - Each entry into compiled code pushes a record onto a JIT-frame list in the VM.
  - The record holds the code object and a slot where the safe-point helper stores the return offset and a pointer to the spill area before calling into Rust.
  - Machine code fills the slot as part of the call sequence, through a `Ctx` field.
  - The collector iterates the list, maps each record's `return_offset` through the function's `GcMap`, and traces the listed locations.

  This keeps the "nothing scans the native stack" rule. A record is the JIT's equivalent of a `Frame`.
- **Values in machine registers.** Cranelift does not give stack maps for arbitrary registers across calls in the way needed here. The simplest rule is that every live heap value is spilled to the spill area before a safe-point call, and listed as `Location::Spill`. `MachineReg` exists for a later register-preserving scheme. The first implementation should not use it at safe points.
- **Non-moving helps.** The collector never moves objects, so traced locations are only read, never updated. A moving collector reopens ADR 0019 (J6).
- **Contract test.** `a_collection_under_optimised_frames_traces_values_in_machine_registers`: run GC-stress (`heap.stress = true`) over code that keeps an object only in a spilled location across an allocating call, and check that it survives.

## 5. Safe points

- **A safe point is any call from machine code into Rust that can allocate or run JavaScript.** In the baseline these are:
  - `Runtime::slow` (any instruction);
  - `Runtime::poll` (interrupt polls and attention at back-edges, function entry and after returns, J7);
  - every exit, which returns to `Vm::jit_run`.
- **Not safe points:** `Runtime::arith`, which is pure, and anything an optimised tier adds that provably cannot allocate. Tag them as such in the code generator, and assert that they do not allocate in debug builds (compare `Heap::alloc_serial` before and after).
- **Attention polls are safe points with a deopt option.** If the attention word requests the debugger, the poll deopts (section 3). If it requests a GC, the poll collects with the GC map in force.
- **Every safe point has a `SafePoint` entry and every side exit has a `FrameState`.** The code generator emits both from the same per-instruction state, so they cannot disagree.

## 6. The birth-serial bound under JIT frames

TASK-100.16 lets a collection run under native frames. A nested run loop records the heap's allocation serial (`Vm::retain_from`), and the collection treats every value born before it as a root. That covers whatever Rust locals hold below the loop.

- **Baseline: unaffected.** Compiled code never recurses on the native stack. Calls exit to `Vm::jit_run`, which runs inside the same run loop as the interpreter, so `native_depth`, `run_depth` and `retain_from` change exactly as they do without the JIT. A native that calls back into JavaScript enters a nested `Vm::run`, and the compiled code under it is just another frame of that loop.
- **Optimised code that calls Rust which runs JavaScript.** Example: a direct call into a native that calls back. The native's nested run sets a new bound. Values the optimised frame holds in spill slots were born before that bound, so they are retained even without a map, but only while the nested loop runs. They are traced by the GC map at any collection, nested or not. The two mechanisms compose: the bound covers Rust locals, the map covers machine locations.
- **Direct calls between compiled functions** put machine frames on the native stack with no run loop between them. Every such frame must be on the JIT-frame list (section 4). A collection in the callee traces the callers through their maps. `retain_from` does not change, because no Rust code with untracked locals sits between the frames. If an optimised tier ever adds Rust code with heap values in locals between two machine frames, that Rust code must either root them (the scoped root stack) or enter a nested run so the bound applies.
- **Contract test.** `a_nested_run_under_optimised_code_keeps_the_birth_serial_bound`: optimised code calls a native that runs a callback which allocates until a collection. Values held in the optimised frame, and values the native holds in locals, both survive.

## 7. Acceptance for the reserved work

1. The four ignored tests in `handoff.rs` pass and are no longer ignored.
2. `crates/agnijs-js-vm/tests/jit_differential.rs` passes with an optimisation that needs each piece, for example registers kept in machine registers, and with GC stress.
3. test262 passes with `AGNIJS_JIT=force`, as the baseline does.
4. No new `unsafe` outside `agnijs-js-jit::exec` (or a sibling module with the same rule), and no stack scanning.
