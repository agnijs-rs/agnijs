//! The AgniJS baseline JIT (ADR 0019, ADR 0020, TASK-101.3).
//!
//! A baseline tier in the Sparkplug style: one function's bytecode becomes
//! one machine-code function that keeps the interpreter's frame exactly.
//! The register file, the frame and the accumulator mean what they mean to
//! the interpreter; compiled code only runs the same instructions without
//! the dispatch. What it cannot do inline it hands back, one instruction
//! at a time, to the interpreter's generic semantics through
//! [`Runtime::slow`], with the frame written back first (ADR 0019 J1, J3).
//!
//! Since TASK-101.3.1 compiled code keeps the function's hottest registers
//! in machine state across call outs and may hold some of them unboxed as
//! numbers on a guess ([`Speculation`]). So the frame in memory is not
//! always complete while machine code runs, and two pieces make that safe
//! ([`handoff`]): at every call out the registers held outside the file are
//! spilled into the compiled frame and described by a point the runtime
//! can read through a [`FrameHandle`] (the GC map: a collection traces the
//! spilled heap values, and nothing scans the native stack), and a guard
//! that finds a guess wrong leaves for the interpreter through a side exit
//! that materialises the frame first ([`Runtime::materialize`]). Control
//! leaves compiled code only at instruction boundaries where the frame has
//! been made the interpreter's ([`Exit`]).
//!
//! Since TASK-101.3.3 compiled code also does three things the runtime
//! did for it, when the runtime describes where they happen
//! ([`CallLayout`]): it calls a compiled function itself, pushing the
//! callee's frame on the runtime's own frame stack and popping it on the
//! return ([`CalleeTable`]; the frame stack stays the single one of ADR
//! 0019 J1), it reads and writes environment slots ([`EnvLayout`]), and it
//! reads global bindings ([`GlobalSite`]). None of them speculates: every
//! guard that fails runs the instruction by the runtime at the same pc.
//!
//! Everything here is safe code except [`exec`]-memory handling, the entry
//! trampoline, the helper trampolines, the frame handle's reads of the
//! spill area and the probes of how a vector, a reference count and an
//! option are laid out, which live in one small module (ADR 0019 J10).

#![deny(unsafe_code)]

use std::marker::PhantomData;
use std::rc::Rc;
use std::time::Duration;

use agnijs_js_compiler::bytecode::{CodeBlock, CodeKind, IcSite, Instr};
use agnijs_js_parser::ast::FunctionKind;

mod analysis;
mod calls;
#[cfg(jit_host)]
mod codegen;
mod exec;

pub use analysis::{live_registers, number_candidates};
pub mod handoff;

pub use calls::{Callee, CalleeTable, GlobalSite, MAX_FILLS, MAX_GLOBAL_FILLS};
pub use exec::FrameHandle;
pub use handoff::{DeoptReason, FrameState, Location, Point, PointKind, Points, SideExit};

/// What the runtime guesses about a block for the code generator to build
/// on (TASK-101.3.1). A wrong guess costs a side exit ([`PointKind::Exit`])
/// and, as the runtime decides, a recompilation without it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Speculation {
    /// Bit `r` set: register `r` (below 64) holds a number for the whole
    /// run, so compiled code keeps it unboxed and reads it without a tag
    /// test. A store of anything else into it, or entering compiled code
    /// while it holds anything else, is a guard failure.
    pub numbers: u64,
}

impl Speculation {
    pub const NONE: Speculation = Speculation { numbers: 0 };
}

/// The layout of a value as compiled code reads and writes it in the
/// register file and the accumulator: sixteen bytes, a one-byte tag at
/// offset 0 and an eight-byte payload at offset 8 (ADR 0019 J5). The VM's
/// `Value` declares the same constants; [`Compiled::compile`] checks a
/// value type against them before it generates code for it.
pub mod layout {
    pub const SIZE: usize = 16;
    pub const TAG_OFFSET: usize = 0;
    pub const PAYLOAD_OFFSET: usize = 8;
    pub const TAG_UNDEFINED: u8 = 0;
    pub const TAG_NULL: u8 = 1;
    pub const TAG_BOOL: u8 = 2;
    pub const TAG_NUMBER: u8 = 3;
    pub const TAG_STRING: u8 = 4;
    pub const TAG_SYMBOL: u8 = 5;
    pub const TAG_BIGINT: u8 = 6;
    pub const TAG_OBJECT: u8 = 7;
    pub const TAG_EMPTY: u8 = 8;
}

/// Where compiled code finds objects, for the inline-cache hit paths of
/// named and keyed property access (TASK-101.3.2). The runtime computes it
/// once, in safe code, from its own types, and passes `None` to
/// [`Compiled::compile_with`] when any check fails; compiled code then
/// calls out for every property access, as without a layout.
///
/// The layout and the [`HeapView`] are the one contract compiled code
/// trusts without checking: it reads objects at these offsets directly.
/// That is why the runtime derives every field from its types
/// (`offset_of!`, [`vec_layout`], probes of live values) rather than
/// declaring it. Compiled code never changes an object's structure: its
/// only heap writes store a value, of [`layout`], into a data slot or a
/// dense element that its guards have just shown holds one.
///
/// An object is at `objects + id * obj_stride + obj_offset`, where
/// `objects` is the arena base the runtime reports through
/// [`Runtime::heap`] after every call out (the arena may reallocate while
/// the runtime runs, so compiled code never keeps the base, or an address
/// derived from it, across a call out). The other offsets are from the
/// object's start:
///
/// - `shape`: the `u32` shape id; `dict`: a pointer-sized word, non-zero in
///   dictionary mode; `class`: the class tag byte; `proto`: the prototype's
///   `u32` id, `u32::MAX` for none.
/// - `inline`: the first of `inline_slots` contiguous 16-byte property
///   slots, every one of them initialised; `overflow_ptr`, `overflow_len`:
///   the pointer to the slots after them and their number.
/// - A property slot holds a data property exactly as a register holds the
///   value ([`layout`]), so its tag byte is at most [`layout::TAG_EMPTY`];
///   any other tag byte is an accessor.
/// - `array_length`: the `u32` length of an array (class tag `class_array`).
/// - `elements_ptr`, `elements_cap`, `elements_len`: the words of the dense
///   element vector. Elements are dense exactly when the pointer is non-zero
///   and the capacity word, read as signed, is not negative; a dense
///   element is a 16-byte value, [`layout::TAG_EMPTY`] for a hole.
/// - `arguments_mapped`: a pointer-sized word of an arguments object (class
///   tag `class_arguments`), non-zero when the object is mapped.
/// - `not_receiver`, `not_elements`: bit `t` set when objects with class
///   tag `t` never take an inline path as receivers of a named access, or
///   for their elements (an arguments object counts as such only when it is
///   mapped).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeapLayout {
    pub obj_stride: u32,
    pub obj_offset: u32,
    pub shape: u32,
    pub dict: u32,
    pub class: u32,
    pub proto: u32,
    pub inline: u32,
    pub inline_slots: u32,
    pub overflow_ptr: u32,
    pub overflow_len: u32,
    pub array_length: u32,
    pub elements_ptr: u32,
    pub elements_cap: u32,
    pub elements_len: u32,
    pub arguments_mapped: u32,
    pub class_array: u8,
    pub class_string: u8,
    pub class_arguments: u8,
    pub not_receiver: u64,
    pub not_elements: u64,
}

/// What compiled code reads of the heap besides objects, refreshed by the
/// runtime after every call out ([`Runtime::heap`]).
#[derive(Clone, Copy, Debug)]
pub struct HeapView {
    /// The object arena's base ([`HeapLayout`]), null if there is none.
    pub objects: *mut u8,
    /// The runtime's prototype epoch: a prototype inline-cache entry hits
    /// only in the epoch it was recorded in (ADR 0012).
    pub proto_epoch: u32,
    /// The environment arena's base ([`EnvLayout`]), null if there is
    /// none. Like `objects`, it may move under any call out.
    pub envs: *mut u8,
}

impl HeapView {
    pub const NONE: HeapView = HeapView {
        objects: std::ptr::null_mut(),
        proto_epoch: 0,
        envs: std::ptr::null_mut(),
    };
}

/// Where a vector keeps its buffer pointer, capacity and length, as byte
/// offsets in the vector itself (see [`vec_layout`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VecLayout {
    pub ptr: u32,
    pub cap: u32,
    pub len: u32,
}

/// The layout of `Vec<T>` on this build, found by inspecting a vector of
/// three copies of `sample`; `None` if it is not three distinct words.
pub fn vec_layout<T: Copy>(sample: T) -> Option<VecLayout> {
    exec::vec_layout(sample)
}

/// [`vec_layout`] for an element type there is no sample of, found on an
/// empty vector with spare capacity.
pub fn vec_layout_of<T>() -> Option<VecLayout> {
    exec::vec_layout_of::<T>()
}

/// The word an `Rc<T>` is, which is the address of its strong count: the
/// identity of a code block in the [`CalleeTable`], and what compiled code adds
/// one to when it pushes a frame holding a reference to the block
/// ([`CallLayout`]). `None` if this build's `Rc` is not laid out that way
/// (a probe of a live `Rc` decides), and then nothing is called directly.
/// The pointer is for identity and for [`Callee::code`]; the runtime never
/// reads through it.
pub fn rc_word<T>(rc: &Rc<T>) -> Option<*const usize> {
    exec::rc_word(rc)
}

/// Where compiled code finds the runtime's frame stack, register file and
/// function objects, so that a `Call` of a compiled function pushes the
/// callee's frame, runs its machine code and pops the frame without
/// leaving compiled code (TASK-101.3.3). Like [`HeapLayout`], the runtime
/// computes it once, in safe code, from its own types (`offset_of!`,
/// [`vec_layout_of`], address differences in live values), and passes
/// `None` to [`Compiled::compile_calling`] when any check fails; every
/// call then goes through [`Runtime::call`]. Needs a [`HeapLayout`] too.
/// Compiled code calls whatever the runtime's [`CalleeTable`] holds from
/// the run it is in, so a runtime compiles every block it puts in one
/// table with one layout.
///
/// A frame is `frame_size` bytes in the frame vector ([`CallView::frames`]).
/// Compiled code pushes one by copying the image of it the callee's entry
/// in the [`CalleeTable`] holds and then writing only these fields, each
/// with a value of the field's own type:
///
/// - `frame_base`: a pointer-sized word, the index in the register file of
///   the frame's first register;
/// - `frame_this`: a value ([`layout`]);
/// - `frame_argc`: a pointer-sized word, the number of arguments;
/// - `frame_callee`: the `u32` id of the function object, inside a field
///   the image has already made "some function";
/// - `frame_env`, `frame_base_env` (`env_size` bytes each, 4 or 8) and
///   `frame_module` (`module_size` bytes): copied whole from the function's
///   data at `fn_env` and `fn_module`, which have the same types.
///
/// Of the caller's frame it writes `frame_pc` (a pointer-sized word: the
/// instruction after the call) and `frame_acc` (a value), which is the
/// write-back the runtime does before a call (ADR 0019 J1). It reads
/// `frame_code` (the frame's reference to its code block, an `Rc` word,
/// [`rc_word`]) and `frame_realm` (`u32`) when it pops a frame.
///
/// A function object is an object ([`HeapLayout`]) with class tag
/// `class_function`, whose pointer-sized word at `function_data` (from the
/// object's start) addresses its data: `fn_code` (the `Rc` word of its code
/// block), `fn_env`, `fn_module`, `fn_realm` (`u32`) and `fn_this_mode`
/// (one byte, `this_mode_global` when a null or undefined `this` becomes
/// the realm's global object and a primitive one is boxed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallLayout {
    /// The words of the frame vector and of the register vector.
    pub frames: VecLayout,
    pub registers: VecLayout,
    pub frame_size: u32,
    /// Frames the stack may hold; a call that would exceed it is the
    /// runtime's (it raises the stack overflow).
    pub max_frames: u32,
    pub frame_code: u32,
    pub frame_pc: u32,
    pub frame_base: u32,
    pub frame_acc: u32,
    pub frame_this: u32,
    pub frame_argc: u32,
    pub frame_callee: u32,
    pub frame_env: u32,
    pub frame_base_env: u32,
    pub env_size: u32,
    pub frame_module: u32,
    pub module_size: u32,
    pub frame_realm: u32,
    pub class_function: u8,
    pub function_data: u32,
    pub fn_code: u32,
    pub fn_env: u32,
    pub fn_module: u32,
    pub fn_realm: u32,
    pub fn_this_mode: u32,
    pub this_mode_global: u8,
    /// Compiled code makes calls itself. (Without it the layout still
    /// serves what else compiled code reads through it.)
    pub direct: bool,
    /// Environment records, when compiled code may read and write their
    /// slots itself (`LdaEnv`, `StaEnv`): it finds the running frame's
    /// innermost environment in the frame, at `frame_env`.
    pub env: Option<EnvLayout>,
    /// The runtime keeps a stamp for its global bindings
    /// ([`Runtime::global_stamp`]), so compiled code may read a global
    /// binding at the address a [`GlobalSite`] holds.
    pub globals: bool,
}

/// Where compiled code finds environment records (TASK-101.3.3), computed
/// by the runtime like [`HeapLayout`]. Environment `id` is at
/// `envs + id * stride + offset`, `envs` being [`HeapView::envs`]. From
/// its start: `kind` is a byte, `declarative` for a record whose bindings
/// are the values ([`layout`]) of a vector with its buffer pointer at
/// `slots_ptr` and its length at `slots_len`, and whose enclosing
/// environment is the optional id at `parent`. Any other kind is the
/// runtime's.
///
/// An optional environment id (in a frame, or a record's parent) is two
/// words of four bytes: a tag whose first byte is `opt_some` when there is
/// an id, at `opt_tag`, and the `u32` id at `opt_id` ([`option_tag`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnvLayout {
    pub stride: u32,
    pub offset: u32,
    pub kind: u32,
    pub declarative: u8,
    pub slots_ptr: u32,
    pub slots_len: u32,
    pub parent: u32,
    pub opt_tag: u32,
    pub opt_some: u8,
    pub opt_id: u32,
}

/// How an `Option<T>` of a four-byte `T` without a niche tells `Some`
/// from `None`, for [`EnvLayout`]: the offset of its tag's first byte and
/// that byte's value for `Some`, given the offset of the payload
/// (`payload`, the difference of two addresses in a `Some`). `None` if
/// the option is not two such words, or the two values show the same byte.
pub fn option_tag<T: Copy>(
    none: &Option<T>,
    some: &Option<T>,
    payload: usize,
) -> Option<(u32, u8)> {
    exec::option_tag(none, some, payload)
}

/// The addresses compiled code pushes and pops frames through
/// ([`CallLayout`]), fixed for a run: the frame vector, the register
/// vector (the vectors themselves, not their buffers, which compiled code
/// re-reads at every use), the current realm (a `u32`) and the table of
/// functions compiled code may call directly ([`CalleeTable::base`]).
#[derive(Clone, Copy, Debug)]
pub struct CallView {
    pub frames: *mut u8,
    pub registers: *mut u8,
    pub realm: *mut u8,
    pub callees: *const u8,
}

impl CallView {
    pub const NONE: CallView = CallView {
        frames: std::ptr::null_mut(),
        registers: std::ptr::null_mut(),
        realm: std::ptr::null_mut(),
        callees: std::ptr::null(),
    };
}

/// A value type compiled code can work on. The constructors give the
/// values compiled code writes itself; [`Compiled::compile`] checks their
/// bytes against [`layout`] and refuses a type that does not match, so an
/// implementation cannot make compiled code write an invalid value.
pub trait JitValue: Copy + 'static {
    fn undefined() -> Self;
    fn null() -> Self;
    fn boolean(b: bool) -> Self;
    fn number(n: f64) -> Self;
    fn empty() -> Self;
}

/// Why compiled code returned to its caller. Apart from [`Exit::Return`],
/// the frame stack is exactly as the interpreter would leave it at an
/// instruction boundary: the caller reloads the top frame and goes on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// The function executed `Return`: [`Outcome::acc`] is the
    /// accumulator and [`Outcome::pc`] the pc after the instruction; the
    /// frame has not been written back or popped.
    Return,
    /// An instruction run by [`Runtime::slow`] pushed or popped frames (a
    /// call, a getter): the top frame is no longer this function's, or is
    /// it at a pc compiled code was not told about.
    Frame,
    /// [`Runtime::slow`] or [`Runtime::poll`] raised a fault, which the
    /// runtime keeps.
    Fault,
    /// [`Runtime::slow`] finished the run (an outcome the runtime keeps).
    Done,
    /// Continue in the interpreter at the frame's pc: the runtime asked
    /// for it (a debugger attached, the attention word is set), or the
    /// entry pc is not one compiled code can start at.
    Interp,
}

/// What a runtime call returns to compiled code.
pub enum Resume<V> {
    /// The instruction completed normally: continue at `pc` with `acc`.
    Next { pc: u32, acc: V },
    /// Leave compiled code; the frame has been written back.
    Exit(Exit),
}

/// The interpreter's side of compiled code: the generic semantics of one
/// instruction, the interrupt poll, and the register window. Implemented
/// by the VM in safe code; called only from compiled code, between
/// instructions, with nothing of the function's state outside the frame
/// but the accumulator passed in.
pub trait Runtime<V: JitValue> {
    /// Run the instruction at `pc` by its generic semantics with
    /// accumulator `acc`: write the frame back (`pc + 1`, `acc`), step,
    /// and report where execution continues. Every instruction's effects
    /// complete before it returns (ADR 0019 J3).
    fn slow(&mut self, pc: u32, acc: V) -> Resume<V>;

    /// [`Runtime::slow`] for a `Call` instruction, through its own entry
    /// so that the runtime can take the call without decoding a general
    /// instruction (and run a compiled callee from here). In code
    /// compiled with a [`CallLayout`], compiled code did not find the
    /// callee in the runtime's table, or could not make the call itself
    /// for another reason; the runtime may add the callee
    /// ([`CalleeTable::fill`]).
    fn call(&mut self, pc: u32, acc: V) -> Resume<V> {
        self.slow(pc, acc)
    }

    /// The function compiled code called directly from the `Call` at `pc`
    /// executed `Return` with `acc`, and compiled code did not finish the
    /// return itself. With `popped` false the callee's frame is still the
    /// top one: pop it by the generic semantics of `Return`. With `popped`
    /// true compiled code has popped it and the caller's frame is the top
    /// one, written back but for the accumulator. Either way the poll
    /// after a call returns (ADR 0019 J7) and any safe point are this
    /// call's to make, and `Next` continues the caller at `pc + 1` with
    /// the value returned.
    ///
    /// Only code compiled with a [`CallLayout`] calls this, so a runtime
    /// that gives one implements it.
    fn returned(&mut self, pc: u32, acc: V, popped: bool) -> Resume<V> {
        let _ = (pc, acc, popped);
        Resume::Exit(Exit::Interp)
    }

    /// The frame stack, the register file and the current realm, for code
    /// compiled with a [`CallLayout`]. Called when compiled code is
    /// entered; the addresses must stay valid, and the vectors and the
    /// realm stay where they are, for the whole run. The default reports
    /// none, and such code is then not entered.
    fn call_view(&mut self) -> CallView {
        CallView::NONE
    }

    /// The epoch the runtime's inline caches are valid in: compiled code
    /// calls a function directly only while its block's caches are of this
    /// epoch ([`Callee::shape_epoch`]). Asked on entry and after every call
    /// out that continues compiled code.
    fn shape_epoch(&self) -> u64 {
        0
    }

    /// The lowest address the native stack pointer may be at when
    /// compiled code calls a function directly: each such call puts a
    /// machine frame on the native stack, and whatever the callee calls
    /// out to runs below it. A call below the floor is the runtime's
    /// ([`Runtime::call`]), which may unwind the machine frames first.
    /// Asked on entry. The default allows no direct call.
    fn stack_floor(&self) -> usize {
        usize::MAX
    }

    /// Compiled code made `n` calls directly in the run that just ended.
    fn direct_calls(&mut self, n: u64) {
        let _ = n;
    }

    /// [`Runtime::slow`] for a global or environment load or store or the
    /// callee (`LdaGlobal`, `LdaEnv`, `StaEnv`, `LdaCallee`), through its own
    /// entry: the common case is a cache hit or a slot read, which a small
    /// function serves at a fraction of the general path's cost.
    ///
    /// `Some` is the new accumulator, the instruction done, and promises
    /// that nothing compiled code caches changed: no allocation, no
    /// interrupt-counter change, no safe point due, the register window
    /// and heap view as they were. `None` runs the instruction by
    /// [`Runtime::slow`] instead.
    fn fast(&mut self, pc: u32, acc: V) -> Option<V> {
        let _ = (pc, acc);
        None
    }

    /// [`Runtime::fast`] for an `LdaGlobal` of code compiled with
    /// [`CallLayout::globals`], whose `site` did not serve it: the runtime
    /// may give the site the binding's address ([`GlobalSite::fill`]), so
    /// that compiled code reads it itself until the stamp moves.
    fn global(&mut self, pc: u32, acc: V, site: Option<&GlobalSite>) -> Option<V> {
        let _ = site;
        self.fast(pc, acc)
    }

    /// Where the runtime keeps the stamp of its global bindings, for code
    /// compiled with [`CallLayout::globals`]: a `u64` that changes
    /// whenever the address of any global binding's value may have (a
    /// binding added or removed, in any realm). It must stay where it is
    /// for the run. The default has none, and such code is then not
    /// entered.
    fn global_stamp(&mut self) -> *const u64 {
        std::ptr::null()
    }
    /// The running frame's `this`, which compiled code loads itself for
    /// `LdaThis` (it is fixed for a frame's life). Asked when code that
    /// has an `LdaThis` is entered; `None` leaves such code unentered.
    fn this_value(&mut self) -> Option<V> {
        None
    }
    /// A loop back-edge from the jump at `pc` to `target` whose interrupt
    /// poll is due (the counter, already advanced, is a multiple of
    /// 0x1000: ask the host), or taken while [`Runtime::safe_point_due`]
    /// said yes (service it). `Next` continues at `target`.
    fn poll(&mut self, pc: u32, target: u32, acc: V) -> Resume<V>;
    /// Whether the runtime wants every back-edge to be a safe point that
    /// calls [`Runtime::poll`], as under GC stress, where the interpreter
    /// collects at every loop head. Asked on entry and after every call out
    /// that continues compiled code.
    fn safe_point_due(&self) -> bool {
        false
    }
    /// The running function's register window (from its base). Compiled
    /// code addresses registers through it until the next runtime call:
    /// the register file may reallocate when frames are pushed.
    fn registers(&mut self) -> &mut [V];
    /// The VM's interrupt-poll counter (ADR 0019 J7); compiled code
    /// advances a copy at back-edges and hands it back at every call out.
    fn interrupt_counter(&self) -> u32;
    fn set_interrupt_counter(&mut self, counter: u32);
    /// `a OP b` on two numbers for the operators compiled code does not
    /// expand itself. Pure: it sees no runtime state, must not panic, and
    /// is called without the frame being written back.
    fn arith(op: Arith, a: f64, b: f64) -> f64;
    /// The object arena and the prototype epoch, for code compiled with a
    /// [`HeapLayout`]. Called when compiled code is entered and after every
    /// call out that continues it, never while it runs. The default reports
    /// no heap, and code compiled with a layout is then not entered.
    fn heap(&mut self) -> HeapView {
        HeapView::NONE
    }
    /// The running block's inline-cache sites (its `Feedback::ic`), for code
    /// compiled with a [`HeapLayout`]; empty if this runtime keeps no
    /// feedback for the block, and code that has sites is then not entered.
    /// Called when compiled code is entered. Compiled code reads the entries
    /// in place for the rest of the run (they are `Cell`s, which the runtime
    /// may update meanwhile), so the table must stay where it is while the
    /// block runs, as a block's feedback does.
    fn ic_table(&mut self) -> &[IcSite] {
        &[]
    }

    /// Compiled code is about to run for the top frame: keep `frame` on the
    /// runtime's list of compiled frames until [`Runtime::frame_left`], so
    /// that a collection under it can trace the heap values the frame holds
    /// outside the register file ([`FrameHandle::live`]) and a debugger can
    /// write them into the interpreter frame ([`FrameHandle::state`]). The
    /// handle reads the compiled frame only while it is on the list, which
    /// is exactly while the machine frame exists. A runtime that keeps no
    /// list must not be given code compiled with a [`Speculation`] or run
    /// under a collector; the baseline contract still holds for the rest.
    fn frame_entered(&mut self, frame: FrameHandle<V>) {
        let _ = frame;
    }

    /// The machine frame given to [`Runtime::frame_entered`] is gone.
    fn frame_left(&mut self) {}

    /// Write `state` into the interpreter frame: every listed register,
    /// and for a side exit the pc and the accumulator. Called before
    /// compiled code is left for any reason but a return (an exit from a
    /// call out, a guard failure, an entry guard), so that the frame the
    /// interpreter reloads is complete. Materialising allocates nothing.
    /// The default writes the registers only.
    fn materialize(&mut self, state: FrameState<V>) {
        let regs = self.registers();
        for (r, v) in state.regs {
            regs[r as usize] = v;
        }
    }
}

/// The number operators compiled code calls [`Runtime::arith`] for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Arith {
    /// `%` (ECMAScript `Number::remainder`).
    Mod = 0,
    /// `**` (`Number::exponentiate`).
    Exp = 1,
}

/// Where a run of compiled code ended.
pub struct Outcome<V> {
    pub exit: Exit,
    /// For [`Exit::Return`]: the pc after the `Return`.
    pub pc: u32,
    /// For [`Exit::Return`]: the accumulator.
    pub acc: V,
}

/// Why a code block has no compiled code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ineligible {
    /// No executable memory or no code generator on this platform, or the
    /// start-up probe failed (ADR 0020).
    Unavailable,
    /// The value type does not have the [`layout`] compiled code assumes.
    Layout,
    /// Generators, async functions, class constructors, modules and eval
    /// code stay in the interpreter.
    Kind,
    /// An instruction the baseline tier does not handle, at this pc.
    Instruction(u32),
    TooLarge,
    /// The code generator refused the function (a bug).
    Compile(String),
}

/// The largest block compiled (instructions). Larger blocks are rarely
/// hot and take long to compile.
pub const MAX_INSTRS: usize = 20_000;

/// Whether `code` can be compiled: a check on the block alone, before any
/// code is generated. Generators and async functions suspend their frames
/// and derived class constructors bind `this` late (a base class's
/// constructor has its `this` from the start, like any function called
/// with `new`); eval and module code run once;
/// direct eval and `super()` inspect the frame in ways compiled code does
/// not model; `debugger` belongs to the interpreter (ADR 0019 J9).
pub fn eligible(code: &CodeBlock) -> Result<(), Ineligible> {
    if code.function_kind != FunctionKind::Normal
        || !matches!(
            code.kind,
            CodeKind::Script
                | CodeKind::Function
                | CodeKind::Arrow
                | CodeKind::Method
                | CodeKind::ClassConstructor
        )
        || code.is_derived_constructor
    {
        return Err(Ineligible::Kind);
    }
    if code.instrs.is_empty() || code.instrs.len() > MAX_INSTRS {
        return Err(Ineligible::TooLarge);
    }
    for (pc, instr) in code.instrs.iter().enumerate() {
        if matches!(
            instr,
            Instr::InitialYield
                | Instr::Yield { .. }
                | Instr::YieldRaw { .. }
                | Instr::Await
                | Instr::ResumeDispatch { .. }
                | Instr::Debugger
                | Instr::CallEval { .. }
                | Instr::CallEvalSpread { .. }
                | Instr::SuperCall { .. }
                | Instr::SuperCallSpread { .. }
        ) {
            return Err(Ineligible::Instruction(pc as u32));
        }
    }
    Ok(())
}

/// Whether compiled code can be expected to beat the interpreter on
/// `code`. Every instruction compiled code does not expand leaves machine
/// code for the runtime ([`Runtime::slow`]), which costs more than the
/// interpreter's own dispatch of the same instruction: a call out, the
/// frame handed over, a second decode. Calls cost more still, since they
/// leave compiled code altogether and re-enter it when the callee returns.
/// So a block is worth compiling only when most of its instructions run
/// inline: at most one in [`CALL_OUT_SHARE`] calls out, counting a call
/// as two. A tiering policy applies this; forced compilation (for tests)
/// does not.
pub fn profitable(code: &CodeBlock) -> bool {
    profitable_with(code, false)
}

/// [`profitable`], for code compiled with a [`HeapLayout`] when `heap`:
/// named and keyed property accesses then run inline on a cache hit.
///
/// A block without a loop is not worth it either: it is entered once per
/// call, and entering compiled code (and leaving it for every call the
/// block makes) costs more than a short block's dispatch saves.
pub fn profitable_with(code: &CodeBlock, heap: bool) -> bool {
    profitable_for(code, heap, None)
}

/// Whether `code` has a loop (a backward branch).
pub fn has_loop(code: &CodeBlock) -> bool {
    code.instrs
        .iter()
        .any(|i| codegen_branch_offset(i).is_some_and(|o| o <= 0))
}

/// [`profitable_with`], for code compiled with a [`CallLayout`] when
/// `calls`: what the layout lets compiled code do itself costs nothing
/// (environment slots, global bindings), and a `Call` costs one call out,
/// not two, since a call of a compiled function is made in machine code.
/// (What a site calls is not in the bytecode. A loop that calls natives is
/// then judged worth compiling when it is not; the runtime's judgement of
/// the running code gives it up.) With direct calls a block without a loop
/// can be worth compiling too: it is entered by machine code, not through
/// the runtime, once its callers are compiled.
pub fn profitable_for(code: &CodeBlock, heap: bool, calls: Option<&CallLayout>) -> bool {
    let direct = heap && calls.is_some_and(|c| c.direct);
    if !direct && !has_loop(code) {
        return false;
    }
    let envs = heap && calls.is_some_and(|c| c.env.is_some());
    let globals = heap && calls.is_some_and(|c| c.globals);
    let mut cost = 0usize;
    for instr in code.instrs.iter() {
        cost += match instr {
            i if expands_inline(code, i) => 0,
            Instr::GetNamed { ic, .. } | Instr::SetNamed { ic, .. } if heap => named_site_cost(code, *ic),
            Instr::GetKeyed { .. } | Instr::SetKeyed { .. } if heap => 0,
            Instr::LdaEnv { .. } | Instr::StaEnv { .. } if envs => 0,
            Instr::LdaGlobal(_) if globals => 0,
            Instr::LdaThis => 0,
            Instr::Call { .. } if direct => 1,
            Instr::Call { .. }
            | Instr::CallSpread { .. }
            | Instr::Construct { .. }
            | Instr::ConstructSpread { .. } => 2,
            _ => 1,
        };
    }
    cost * CALL_OUT_SHARE <= code.instrs.len()
}

/// See [`profitable`].
pub const CALL_OUT_SHARE: usize = 6;

/// The [`profitable`] cost of named access site `ic`, judged by the
/// feedback the block has so far: 0 when compiled code can run its hits
/// itself, 1 when it calls out (megamorphic, or a kind compiled code does
/// not expand), 2 when it has seen an accessor (a call).
fn named_site_cost(code: &CodeBlock, ic: u16) -> usize {
    use agnijs_js_compiler::bytecode::IcEntry;
    if ic as usize >= code.ic_count as usize {
        return 1;
    }
    let Some(site) = code.feedback.get().and_then(|f| f.ic.get(ic as usize)) else {
        return 0;
    };
    let mut cost = 0;
    for e in site.entries.iter() {
        cost = cost.max(match e.get().kind {
            IcEntry::KIND_EMPTY | IcEntry::KIND_OWN_DATA | IcEntry::KIND_PROTO_DATA | IcEntry::KIND_ARRAY_LENGTH => 0,
            IcEntry::KIND_OWN_ACCESSOR | IcEntry::KIND_PROTO_ACCESSOR => 2,
            _ => 1,
        });
    }
    cost
}

fn codegen_branch_offset(instr: &Instr) -> Option<i32> {
    analysis::branch_offset(instr)
}

/// Whether compiled code expands `instr` itself, at least for numbers
/// (the code generator's inline cases; others call out).
fn expands_inline(code: &CodeBlock, instr: &Instr) -> bool {
    match *instr {
        Instr::LdaConst(c) => matches!(
            code.constants.get(c as usize),
            Some(agnijs_js_compiler::bytecode::Constant::Number(_))
        ),
        Instr::BinRR { op, .. } | Instr::BinRI { op, .. } | Instr::BinAR { op, .. } | Instr::BinAI { op, .. } => {
            !matches!(
                op,
                agnijs_js_compiler::bytecode::BinOp::InstanceOf | agnijs_js_compiler::bytecode::BinOp::In
            )
        }
        Instr::JumpIfCmpRR { op, .. }
        | Instr::JumpIfNotCmpRR { op, .. }
        | Instr::JumpIfCmpRI { op, .. }
        | Instr::JumpIfNotCmpRI { op, .. }
        | Instr::JumpIfCmpRA { op, .. }
        | Instr::JumpIfNotCmpRA { op, .. }
        | Instr::JumpIfCmpAI { op, .. }
        | Instr::JumpIfNotCmpAI { op, .. } => op.is_comparison(),
        Instr::LdaUndefined
        | Instr::LdaNull
        | Instr::LdaTrue
        | Instr::LdaFalse
        | Instr::LdaEmpty
        | Instr::LdaInt(_)
        | Instr::Ldar(_)
        | Instr::Star(_)
        | Instr::Mov { .. }
        | Instr::Nop
        | Instr::Add(_)
        | Instr::Sub(_)
        | Instr::Mul(_)
        | Instr::Div(_)
        | Instr::Mod(_)
        | Instr::Exp(_)
        | Instr::Shl(_)
        | Instr::Shr(_)
        | Instr::UShr(_)
        | Instr::BitAnd(_)
        | Instr::BitOr(_)
        | Instr::BitXor(_)
        | Instr::Eq(_)
        | Instr::Ne(_)
        | Instr::StrictEq(_)
        | Instr::StrictNe(_)
        | Instr::Lt(_)
        | Instr::Le(_)
        | Instr::Gt(_)
        | Instr::Ge(_)
        | Instr::Inc
        | Instr::Dec
        | Instr::ToNumeric
        | Instr::CheckTdz(_)
        | Instr::Jump(_)
        | Instr::JumpIfTrue(_)
        | Instr::JumpIfFalse(_)
        | Instr::JumpIfNullish(_)
        | Instr::JumpIfNotNullish(_)
        | Instr::JumpIfUndefined(_)
        | Instr::JumpIfNotUndefined(_)
        | Instr::JumpIfEmpty(_)
        | Instr::JumpIfNotEmpty(_)
        | Instr::JumpIfObject(_)
        | Instr::JumpIfStrictEq { .. }
        | Instr::Return => true,
        _ => false,
    }
}

/// Whether this process can run compiled code: a supported target and a
/// start-up probe that compiled and ran a function (cached).
pub fn available() -> bool {
    exec::available()
}

/// How executable memory is obtained on this platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecMemoryKind {
    /// Pages mapped read-write, filled, then made read-execute.
    Protect,
    /// Apple silicon: a `MAP_JIT` mapping, written with this thread's JIT
    /// write protection lifted (`pthread_jit_write_protect_np`), which
    /// hardened-runtime processes need (the allow-jit entitlement).
    MapJit,
}

/// The kind of executable memory the most recent mapping used (`None`
/// before any, or where the JIT is unavailable). The probe behind
/// [`available`] makes one.
pub fn exec_memory_kind() -> Option<ExecMemoryKind> {
    exec::memory_kind()
}

#[cfg(jit_host)]
thread_local! {
    /// The call thunks made on this thread, by value type and layouts.
    #[allow(clippy::type_complexity)]
    static THUNKS: std::cell::RefCell<Vec<(std::any::TypeId, HeapLayout, CallLayout, Rc<exec::ExecMemory>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The call thunk for a runtime's layouts (`codegen::compile_thunk`), made
/// once per thread; `None` if it cannot be made, and calls are then the
/// runtime's.
#[cfg(jit_host)]
fn call_thunk<V: JitValue>(
    code: &CodeBlock,
    heap: &HeapLayout,
    calls: &CallLayout,
) -> Option<Rc<exec::ExecMemory>> {
    let key = std::any::TypeId::of::<V>();
    THUNKS.with(|thunks| {
        let mut thunks = thunks.borrow_mut();
        if let Some(t) = thunks
            .iter()
            .find(|t| t.0 == key && t.1 == *heap && t.2 == *calls)
        {
            return Some(t.3.clone());
        }
        let bytes = codegen::compile_thunk(code, &exec::offsets::<V>(), heap, calls).ok()?;
        let mem = Rc::new(exec::ExecMemory::new(&bytes)?);
        thunks.push((key, *heap, *calls, mem.clone()));
        Some(mem)
    })
}

/// One code block's machine code.
pub struct Compiled<V> {
    #[cfg_attr(not(jit_host), allow(dead_code))]
    mem: exec::ExecMemory,
    /// `entry[pc]`: compiled code can start at `pc`.
    entry: Box<[bool]>,
    /// What the code needs to be entered and what the runtime reads of it
    /// while it runs: the registers it addresses, whether it reads the heap
    /// (compiled with a [`HeapLayout`]) and how many inline-cache sites,
    /// whether it has an `LdaThis`, its safe points, side exits and entry
    /// guards ([`handoff`]) and its global sites. Shared with the frame
    /// handles of running frames.
    info: Rc<exec::CodeInfo>,
    /// Size of the machine code in bytes.
    pub code_size: usize,
    /// Time spent generating it.
    pub compile_time: Duration,
    _value: PhantomData<fn(V) -> V>,
}

impl<V: JitValue> Compiled<V> {
    /// Compile `code`, which must be [`eligible`].
    pub fn compile(code: &CodeBlock) -> Result<Self, Ineligible> {
        Self::compile_with(code, None)
    }

    /// Compile `code`, with the inline-cache hit paths when the runtime
    /// describes its heap (`heap`); see [`HeapLayout`].
    pub fn compile_with(code: &CodeBlock, heap: Option<&HeapLayout>) -> Result<Self, Ineligible> {
        Self::compile_speculating(code, heap, &Speculation::NONE)
    }

    /// [`Compiled::compile_with`], building on the runtime's guesses.
    pub fn compile_speculating(
        code: &CodeBlock,
        heap: Option<&HeapLayout>,
        spec: &Speculation,
    ) -> Result<Self, Ineligible> {
        Self::compile_calling(code, heap, None, spec)
    }

    /// [`Compiled::compile_speculating`], with calls of compiled functions
    /// made in machine code when the runtime describes its frame stack
    /// (`calls`, which needs `heap` too); see [`CallLayout`].
    pub fn compile_calling(
        code: &CodeBlock,
        heap: Option<&HeapLayout>,
        calls: Option<&CallLayout>,
        spec: &Speculation,
    ) -> Result<Self, Ineligible> {
        eligible(code)?;
        if !available() {
            return Err(Ineligible::Unavailable);
        }
        if !exec::layout_matches::<V>() {
            return Err(Ineligible::Layout);
        }
        Self::generate(code, heap, calls.filter(|_| heap.is_some()), spec)
    }

    #[cfg(jit_host)]
    fn generate(
        code: &CodeBlock,
        heap: Option<&HeapLayout>,
        calls: Option<&CallLayout>,
        spec: &Speculation,
    ) -> Result<Self, Ineligible> {
        let start = std::time::Instant::now();
        // The global sites exist before the code does: it addresses them.
        let globals: Box<[GlobalSite]> = match calls {
            Some(c) if c.globals => code
                .instrs
                .iter()
                .filter(|i| matches!(i, Instr::LdaGlobal(_)))
                .map(|_| GlobalSite::default())
                .collect(),
            _ => Box::new([]),
        };
        // One call thunk for the layouts: every call site calls it.
        let has_call = code.instrs.iter().any(|i| matches!(i, Instr::Call { .. }));
        let thunk = match (heap, calls) {
            (Some(h), Some(c)) if c.direct && has_call => call_thunk::<V>(code, h, c),
            _ => None,
        };
        let direct = calls.map(|layout| codegen::Calls {
            layout,
            globals: &globals,
            thunk: thunk.as_ref().map_or(0, |t| t.address()),
        });
        let lowered = codegen::compile(code, &exec::offsets::<V>(), heap, direct.as_ref(), spec)
            .map_err(Ineligible::Compile)?;
        let mem = exec::ExecMemory::new(&lowered.bytes).ok_or(Ineligible::Unavailable)?;
        let mut entry = vec![false; code.instrs.len()].into_boxed_slice();
        for &pc in &lowered.entries {
            entry[pc as usize] = true;
        }
        Ok(Compiled {
            mem,
            entry,
            info: Rc::new(exec::CodeInfo {
                registers: code.register_count as usize,
                heap: heap.is_some(),
                ic: if heap.is_some() {
                    code.ic_count as usize
                } else {
                    0
                },
                this: code.instrs.iter().any(|i| matches!(i, Instr::LdaThis)),
                calls: calls.is_some(),
                envs: calls.is_some_and(|c| c.env.is_some()),
                stamp: calls.is_some_and(|c| c.globals),
                direct: lowered.cached == 0,
                points: lowered.points,
                globals,
                _thunk: thunk,
            }),
            code_size: lowered.bytes.len(),
            compile_time: start.elapsed(),
            _value: PhantomData,
        })
    }

    #[cfg(not(jit_host))]
    fn generate(
        _code: &CodeBlock,
        _heap: Option<&HeapLayout>,
        _calls: Option<&CallLayout>,
        _spec: &Speculation,
    ) -> Result<Self, Ineligible> {
        Err(Ineligible::Unavailable)
    }

    /// The code's safe points, side exits and entry guards.
    pub fn points(&self) -> &Points {
        &self.info.points
    }

    pub(crate) fn info(&self) -> &Rc<exec::CodeInfo> {
        &self.info
    }

    /// The address of the machine code.
    pub(crate) fn entry_address(&self) -> usize {
        self.mem.address()
    }

    /// Whether machine code may call this code directly
    /// ([`CalleeTable::fill`]): it keeps no register in machine state, so
    /// its frame needs no [`FrameHandle`] for the collector or a debugger.
    pub fn direct(&self) -> bool {
        self.info.direct
    }

    /// Whether compiled code can start at `pc`: the function's start, a
    /// loop header, or the instruction after one that may call out.
    #[inline]
    pub fn is_entry(&self, pc: usize) -> bool {
        self.entry.get(pc).copied().unwrap_or(false)
    }

    /// Run from `pc` (an entry, else [`Exit::Interp`] at once) with the
    /// accumulator `acc`, the top frame being this function's.
    #[inline]
    pub fn enter<R: Runtime<V>>(&self, rt: &mut R, pc: u32, acc: V) -> Outcome<V> {
        if !self.is_entry(pc as usize) {
            return Outcome {
                exit: Exit::Interp,
                pc,
                acc,
            };
        }
        exec::enter(&self.mem, &self.info, rt, pc, acc)
    }
}
