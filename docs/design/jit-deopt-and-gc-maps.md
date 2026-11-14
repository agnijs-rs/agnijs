# JIT deoptimization and GC maps: the hand-off specification

Status: implemented (TASK-101.3.1). Sections 1 to 7 are the specification the work was done against; section 8 records what was built where it differs in detail, and section 9 the calls machine code makes itself (TASK-101.3.3). The interfaces are in `crates/agnijs-js-jit/src/handoff.rs`, with the contract tests that were ignored until the work landed; the mechanism is in `exec.rs` (the only `unsafe`), the analysis in `analysis.rs`, the code generation in `codegen.rs`, and the VM's side in `crates/agnijs-js-vm/src/interp/jit.rs`. ADR 0020's addendum on this task is the short form.

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
| Direct calls between compiled functions (machine frames on the native stack) | I1, and the frame stack model | both, plus section 6. Not when the callee holds nothing in machine state and the frame stack stays the interpreter's: that is built (section 9). |
| Inlining, or speculation on call targets or types without a generic fallback | I2 | deopt (section 3) |

Inline IC hit paths with a full miss path need neither piece. They are implemented (TASK-101.3.2, below).

### 1.1 Inline IC hits in the baseline (TASK-101.3.2)

Compiled code reads the object arena directly for named loads and stores that hit their inline cache (monomorphic up to the 4-way polymorphic form, own data and prototype data), dense array element reads and writes, and array `length`. Every guard is the interpreter's own: the receiver's shape against the cache entry's, and for a prototype hit also the holder's shape and `proto_epoch` against the entry's. Any miss calls `Runtime::slow` for the same instruction at the same pc (I2), so nothing is speculated and no deoptimization exists.

The heap view adds a third invariant, which keeps I1 true for addresses as well as values:

- **I3. No heap address across a call out.** The arena base (`Ctx::objects`) and the prototype epoch (`Ctx::proto_epoch`) are the runtime's (`Runtime::heap`) on entry and are refreshed by the trampoline after every call out that continues compiled code. The code generator loads them from the `Ctx` in each instruction that needs them and never keeps them, or any address or value derived from them, in a machine register across a call. Any call out may allocate (the arena may reallocate and move), collect, or change a prototype; the next instruction sees the new view.

Compiled code never changes an object's structure. It only stores values into data slots and dense elements its guards checked, so the shape tables, the IC entries and the collector's invariants are the interpreter's. A store writes a `Value` and needs no write barrier, because the collector is non-incremental. The layout compiled code assumes (`HeapLayout`) is computed at start-up from the VM's own types by address differences, and code is compiled without heap access when any check fails.

GC attention is serviced inside compiled code: every call out that continues it checks the attention word and collects there (`JitRt::attention`), with the frame written back. Under GC stress (`Runtime::safe_point_due`) every back-edge also calls `Runtime::poll`, so compiled loops collect at each loop head as the interpreter does. The test `heap_values_survive_collections_at_every_call_out_from_compiled_code` (in `crates/agnijs-js-vm/tests/jit_inline_cache.rs`) runs inline loads and stores around allocating calls under `heap.stress` with a collection at every safe point. It checks the result against the interpreter's.

Nothing here touches the birth-serial bound (section 6). (Since TASK-101.3.3 compiled code does recurse on the native stack, with no Rust frame holding a heap value in between; section 9.3 says why the bound is still untouched.)

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

- **Baseline: unaffected.** As first built, compiled code never recursed on the native stack: calls exited to `Vm::jit_run`, which runs inside the same run loop as the interpreter, so `native_depth`, `run_depth` and `retain_from` change exactly as they do without the JIT. A native that calls back into JavaScript enters a nested `Vm::run`, and the compiled code under it is just another frame of that loop. The calls machine code now makes itself, and the callees it runs from inside a call out, leave all three as they were (section 9.3).
- **Optimised code that calls Rust which runs JavaScript.** Example: a direct call into a native that calls back. The native's nested run sets a new bound. Values the optimised frame holds in spill slots were born before that bound, so they are retained even without a map, but only while the nested loop runs. They are traced by the GC map at any collection, nested or not. The two mechanisms compose: the bound covers Rust locals, the map covers machine locations.
- **Direct calls between compiled functions** put machine frames on the native stack with no run loop between them. Every such frame must be on the JIT-frame list (section 4). A collection in the callee traces the callers through their maps. `retain_from` does not change, because no Rust code with untracked locals sits between the frames. If an optimised tier ever adds Rust code with heap values in locals between two machine frames, that Rust code must either root them (the scoped root stack) or enter a nested run so the bound applies.
- **Contract test.** `a_nested_run_under_optimised_code_keeps_the_birth_serial_bound`: optimised code calls a native that runs a callback which allocates until a collection. Values held in the optimised frame, and values the native holds in locals, both survive.

## 7. Acceptance for the reserved work

1. The four ignored tests in `handoff.rs` pass and are no longer ignored.
2. `crates/agnijs-js-vm/tests/jit_differential.rs` passes with an optimisation that needs each piece, for example registers kept in machine registers, and with GC stress.
3. test262 passes with `AGNIJS_JIT=force`, as the baseline does.
4. No new `unsafe` outside `agnijs-js-jit::exec` (or a sibling module with the same rule), and no stack scanning.

## 8. As built (TASK-101.3.1)

The implementation follows sections 2 to 6 with these differences and specifics.

- **One table, not two.** `GcMap` and the per-exit `FrameState` tables became one `Points` table per block (`Point { pc, kind, regs }`, kinds `SafePoint { next }`, `Exit { guard }`, `Entry { guard }`). A safe point and a side exit list the same thing: the registers whose value is outside the register file. Machine code stores the point id into `Ctx::point` before a call out or an exit, instead of the runtime mapping a return address: no return-address register and no lookup by offset. `Ctx::spill` holds the compiled frame's spill area, written once at entry; locations are offsets into it (`Location::Spill`, a boxed `Value`). There is no location for the bits of an unboxed number, and `FrameState::raw` of section 3 was not built: see "What is materialised" below. `MachineReg` is unused, as section 4 anticipated.
- **`FrameHandle`, not a record with a code object.** `Runtime::frame_entered` receives a handle over the `Ctx` (a raw pointer, valid until `frame_left`) and the block's `Rc<Points>`. `FrameHandle::live` traces the listed heap values, `FrameHandle::state` reads the whole `FrameState`. The VM keeps the handles in `JitState::frames` with the interpreter frame index and window base, and `gc.rs` walks them after `Vm::regs`.
- **Materialisation at every unexpected continuation.** The call-out trampoline knows the point's successors (`SafePoint::next`). When the runtime continues at any other pc (an exception handled in the same frame, a debugger step, `Exit::Interp` for a fault or a debugger), it materialises the frame first and leaves with `Exit::Interp` rather than continuing in machine code: the interpreter then runs from a complete frame. So compiled code is never entered at a pc that is not an entry, and never continues in machine state the runtime did not expect.
- **Deopt trampoline.** A failed guard calls `Ctx::deopt` with the point id. It builds the `SideExit` (pc, acc, `DeoptReason::Guard`, the guarded register), materialises, and returns `EXIT_INTERP`. Panics in any trampoline are caught and resumed in `enter`, as before.
- **Debugger.** Attaching materialises every frame on the list (`Vm::jit_materialize_all`); such a frame is marked, so the call out it is waiting in does not overwrite what the debugger wrote. Compiled code is not entered while attached, as before, and leaves at its next call out.
- **What is cached.** `analysis::choose_cache` picks up to `MAX_CACHED` (8) registers that loops read or write inline, by use count, speculated ones first. A block without loops caches nothing.
- **Liveness, and where a guard is spent.** `analysis::liveness` computes, per pc, which cached registers may be read before written from there. It follows normal control flow and, since TASK-103.34, the exception edges: every instruction inside a handler's range that can reach the runtime (it calls out, or it is a back-edge, which polls) has the handler's target as a successor. That edge leaves from before the instruction, so the instruction's own write does not kill it. Only the instructions that never call out (`analysis::always_inline`: constants, `Ldar`, `Star`, `Mov`, `LdaThis`, forward jumps, `Return`) have no such edge. So a local a `catch` block reads is live at every instruction of the `try` block that can throw, and at every entry before it. The code generator guards a speculated register when it loads it (at an entry, and after a generic instruction, which may have written any register) only where it is live. One that is dead there is loaded without a test.
- **What is materialised: a register is always a value (TASK-103.34).** A speculated register is two machine words, like any cached register: the payload, which the instructions that compute with it read as an `f64` without a tag test, and the tag word that payload came with. The tag is a number's once the register has been guarded or written by compiled code. After a load without a guard it is the tag word the register file held, until the register is written. Wherever compiled code moves the register without computing with it, it moves both words: the spill at a point, the write-back before a call out that reads it, a copy into the accumulator or another register. So what a point lists for a register is its value, whatever became of the guess: a register nothing has guarded yet reaches the interpreter frame, a handler, the collector and a debugger as what it held when compiled code was entered. Nothing the runtime reads through a `FrameHandle` is boxed from bits. The rule this replaces listed such a register as the bits of a number (`Location::SpillNumber`) and relied on liveness to make that invisible; liveness had no exception edges, so a throw before the first assignment showed a handler the payload bytes of `undefined`, or of whatever the register held before, as a number. Where every path to a point has guarded or written the register, the tag is one constant and costs nothing; elsewhere it is one more value merged at the join.
- **What the VM guesses.** `Vm::jit_guess`: `number_candidates` (registers below 64 read by inline arithmetic or comparison inside a loop) minus `Tier::demoted`, kept only where the live frame holds a `Number`, or holds `Undefined` and is dead at the frame's pc (`agnijs_js_jit::live_registers`, the same analysis: a local not yet assigned that is written before anything reads it). An unassigned register that is live there is not guessed: it would fail the entry guard at once. A guard failure demotes the register and drops the code (`Tier::code` is a `RefCell<Option<Rc<dyn Any>>>` now; every run holds its own `Rc`), and the block is compiled again at its next offer; after `MAX_DEOPTS` (4) failures it stops guessing. `Invalidated` is defined but nothing raises it yet: there is no speculation on prototypes or globals.
- **Tests.** `handoff::tests` (mock runtime with a mock heap, birth-serial bound, fault and panic injection, debugger attach, a register taken unguarded listed and written back as what it held), `exec::tests` (handle reads, Miri-runnable), `analysis::tests` (the exception edges among them), and the VM's `tests/jit_deopt.rs` (speculation, guard exits, recompilation, giving up, collections under a callback from a speculated frame, locals not yet assigned read by a handler after a throw in a speculated loop; run also under `AGNIJS_GC_STRESS=1`).

## 9. Calls in machine code (TASK-101.3.3)

A `Call` of a compiled function is made without leaving compiled code: machine code pushes the callee's interpreter frame, calls the callee's machine code on the native stack and pops the frame when it returns. This is the "direct calls between compiled functions" row of section 1, for the case that needs neither deoptimization nor a GC map: the frame stack stays the interpreter's, and the callee holds nothing in machine state. ADR 0020's addendum on the task is the short form. The code is `Lower::thunk` in `codegen.rs`, `calls.rs`, the `Shared` and `Ctx` halves of the context in `exec.rs`, and `JitRt` in the VM's `interp/jit.rs`.

### 9.1 What machine code is trusted with

Two invariants join I1 to I3. They keep the frame stack the single source of truth (J1) while machine code writes to it.

- **I4. A frame machine code pushes is a frame the runtime built.** The runtime describes its frame stack, register file and function objects once, by `offset_of!` and address differences (`CallLayout`, computed like `HeapLayout`). For each callee it gives the callee table an *image*: a `Frame` it built in safe code for a plain call of a function of that code block in that realm. Machine code pushes a frame by copying the image's bytes into spare capacity of the frame vector and then writing only the fields that differ per call, each with a value of the field's own type read from the function object or the registers: the window's base, `this`, the argument count, the callee's id, the environment (twice) and the module. The image holds a reference to the code block, and the copy takes one by adding one to the count at the `Rc`'s word, which a probe of a live `Rc` checked is where the count is. So every frame in `Vm::frames` is a well-formed `Frame` whoever pushed it, and Rust drops it as usual when an unwind pops it.
- **I5. Machine code never grows or frees what it does not own.** It pushes only into spare capacity of the frame vector and the register file (a call that needs more is the runtime's), re-reads each vector's buffer and length from its header at every use, and pops a frame only when the code block's count stays above zero (the last reference is the runtime's to drop). The headers and the current realm are fields of the VM, at addresses fixed for a run.

What machine code does at a call is what `push_function_frame` and `do_return` do, in the same order, for the calls it takes: the caller's frame is written back first (pc and accumulator, J1), the function-entry poll and the poll after the return are counted on the interrupt counter (J7), and a callee's inline caches must be of the current shape epoch (what `push_frame` checks). Everything else is a guard that sends the call to `Runtime::call` from the same state:

- a callee that is not a function object, or not in the table for its realm;
- a poll due, or the attention word set (so nothing is called directly under GC stress, a debugger or a fuel budget);
- the native stack below the runtime's floor;
- no room in either vector, or the frame limit reached;
- a primitive `this` for a function that would box it.

### 9.2 What the collector and a debugger see

- **Frames.** Every frame is in `Vm::frames` and every register in `Vm::regs`, as under the interpreter. A function called directly keeps no register in machine state (`Compiled::direct`: no cached register, so no spill area and no guess), so it has no `FrameHandle` and needs none. Its caller may keep registers in machine state: the call's safe point lists them exactly as a call through the runtime does, and the caller's handle is on the list. A function that does keep registers in machine state is never called directly. It is entered from inside the call out (ADR 0020, nested entry), which registers its handle.
- **Leaving.** A callee that leaves compiled code any way but by returning (a fault, a frame it pushed and did not run, the interpreter wanted) answers the caller's machine code with a word, which the call thunk hands to a helper (`left`). The helper materialises the caller's frame and answers with the exit code that word is to the caller (section 9.6), level by level, down to `exec::enter`. The interpreter frames were complete all along, so exceptions unwind as in the interpreter.
- **A debugger.** Attaching sets the attention word, so no further call is made directly, and every frame on the handle list is materialised as before. Frames of functions called directly have nothing to materialise.

### 9.3 The birth-serial bound

Nothing changes it, for two reasons.

- **Direct calls.** Between a caller's machine frame and its callee's there is the call thunk's machine frame and no Rust frame at all, so there is no Rust local to keep alive. This is the case section 6 foresaw ("no Rust code with untracked locals sits between the frames").
- **A callee run from inside a call out.** When an instruction run by the runtime pushes one frame, the runtime runs that frame from inside the call out: its compiled code (`enter_callee`), or the interpreter's own loop when it has none (`interpret_callee`, a run loop that ends when that frame returns). The Rust frames between the caller's machine code and the callee (`call_out`, `JitRt::slow` or `call`, the entry) hold no heap value: the instruction's operands were in the caller's frame, which was written back, and the result goes into the frame. So a collection in the callee needs no retention bound for them, `native_depth` and `run_depth` are not touched, and `retain_from` stays the enclosing loop's.

### 9.4 The native stack

Machine frames now nest on the native stack, so the nesting is bounded by the stack pointer, not by a count of levels. The VM sets a floor `JIT_STACK` (96 KB in an optimised build) below the stack position at which compiled code was first entered with none running, or at the limit of the host's stack budget (`agnijs_js_parser::stack::limit`, `Engine::set_stack_budget`) when that is nearer, so machine frames never nest past what the host allowed the engine. Machine code calls directly only while the stack pointer is above the floor, and the runtime starts a callee from inside a call out only then. A call past the floor leaves compiled code the old way (`Exit::Frame`), which unwinds every machine frame; the frames continue from memory. The floor is per engine, not per run, so natives that re-enter JavaScript under compiled code use the same budget.

### 9.5 Environment slots and global bindings

Two more loads moved into machine code with the same discipline: a guard that fails runs the instruction by the runtime at the same pc (I2), and no address is kept across a call out (I3).

- **Environment slots** (`LdaEnv`, `StaEnv`). The running frame is the top of the frame vector. Its innermost environment, each parent and the slot vector are read through `EnvLayout`; the arena base is in `HeapView` and refreshed with the object arena's. With the `jit` feature `Environment` is `#[repr(u8)]`, so that a declarative record is told by its first byte. Any record on the way that is not declarative, or a missing slot, is the runtime's.
- **Global bindings** (`LdaGlobal`). A site records where the binding's value was when the runtime last resolved it (its slot in the global object's dictionary or in the realm's table of global lexicals), the realm, and the heap's *global stamp*. The stamp moves whenever such an address may have moved: a key added to or removed from any object in dictionary mode, and a global lexical declared. Machine code reads the value there only while the stamp and the current realm are the site's, and takes it only when it is a plain, non-empty value. An accessor installed since shows as a first byte above every value tag and a binding in its dead zone as the empty value, and both are the runtime's.

### 9.6 What a directly called function answers

A compiled function answers whoever ran it with one 32-bit word. At or above `EXIT_BASE` it is an exit code. Below it, it is a bytecode pc: "the runtime continued my frame at this pc, which I have no block for". Only code that keeps nothing in machine state answers so (`CodeInfo::direct`: its frame needs no materialising, so `exec::call_out` passes the runtime's pc through); that is exactly the code machine code calls directly. A pc is a pc of the answering frame's own block. It means nothing to another frame.

The caller's call site, though, reads the word the thunk gives it as one about its own frame: it compares it with its own `pc + 1` to see whether the call returned (`Lower::continue_at`). So the rule at the frame boundary is:

- **The call site is only ever handed a word about the calling frame.** The thunk gives it the caller's `pc + 1` when the callee returned and the frame was popped, what the runtime's own entries answer for the caller's frame (`Runtime::call`, `Runtime::returned`, called with the caller's context), or an exit code.
- **A callee may answer** `EXIT_RETURN` (its `Return` ran: the thunk pops the frame, or has the runtime do it), any exit code, or a pc of its own block.
- **What it answers when it has not returned is translated in the "left" path** (`exec::left_code`, in `left_trampoline`): `EXIT_FRAME`, `EXIT_FAULT`, `EXIT_DONE`, `EXIT_INTERP` and `EXIT_PANIC` say the same thing about the run to every frame and pass; anything else, a pc above all, becomes `EXIT_INTERP`. That is always right: the callee's interpreter frame is at the pc the runtime gave it, the caller's was written back before the call, and the interpreter goes on from the frames as they are.

Before TASK-103.36 the "left" path handed the callee's word on unchanged. A callee leaving at a pc equal to the caller's `pc + 1` would then have made the caller continue after the call as if it had returned, with the callee's frame still on top. No instruction of the VM answers a call out with a pc that is not one of its successors, and a throw caught in the same frame is an exit (`Exit::Fault`), not a pc, so the VM could not get there; the toy runtime of `crates/agnijs-js-jit/tests/calls.rs` answers exactly that pc and shows the caller leaving for the interpreter with both frames intact. The rule no longer depends on what the runtime's instructions happen to answer.
