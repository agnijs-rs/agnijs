//! The engine's only unsafe code (ADR 0019 J10, ADR 0020): executable
//! memory, the entry into machine code, and the trampolines machine code
//! calls back through. Everything else in the engine forbids `unsafe`.
//!
//! The obligations, all discharged here:
//!
//! * Executable memory is never writable and executable at once (W^X): it
//!   is written while read-write, then made read-execute (on Apple silicon,
//!   a `MAP_JIT` mapping written only while this thread's JIT write
//!   protection is lifted), and the instruction cache is synchronised
//!   before the first call.
//! * Machine code is only entered through [`enter`], or called by machine
//!   code that was (below). `enter` gives it a [`Ctx`] and a [`Shared`]
//!   that live on `enter`'s stack frame for the whole run; the register
//!   window has at least as many registers as the code addresses, and the
//!   runtime pointer is the `&mut R` `enter` was given, used only through
//!   that pointer until the run ends.
//! * Machine code writes values only with the tags and payloads of
//!   [`crate::layout`], which [`layout_matches`] checks against the value
//!   type's own constructors before any code for that type is generated.
//! * Code compiled with a [`crate::HeapLayout`] reads the heap only
//!   through the arena bases and prototype epoch in the `Shared`, which are
//!   the runtime's own ([`Runtime::heap`]) on entry and after every runtime
//!   call; it keeps none of them, nor any address or value derived from
//!   them, in a machine register across a call (the code generator reloads
//!   them after each). It reads inline caches from the table
//!   [`Runtime::ic_table`] gave on entry, which has every site it reads.
//! * No panic crosses machine code: each trampoline catches it, parks the
//!   payload in the `Shared`, and returns `EXIT_PANIC`; `enter` resumes the
//!   unwind once machine code has returned. That is every trampoline that
//!   reaches the runtime's state: the generic path, calls and returns, the
//!   polls, the fast paths for bindings and global sites ([`hit_or_out`]),
//!   the side exits and the "left" helper. A frame is not materialised for
//!   a panic: what unwinds is the host's to discard. The one trampoline
//!   that parks nothing is [`Runtime::arith`]'s: it is given two numbers
//!   and no context, sees no state a panic could leave half changed, and
//!   is documented not to panic; should an implementation panic anyway,
//!   the panic is caught there and the result is NaN.
//! * Code compiled with a [`crate::CallLayout`] pushes and pops the
//!   runtime's own frames (TASK-101.3.3). It reads and writes the frame
//!   stack, the register file and the current realm only through the
//!   addresses [`Runtime::call_view`] gave on entry, at the layout's
//!   offsets, and re-reads each vector's buffer and length from its header
//!   at every use: the runtime may grow either under any call. It pushes a
//!   frame only by copying the image the callee's entry in the runtime's
//!   table holds ([`crate::CalleeTable`], filled by the runtime, which
//!   built the image as a frame of its own type in safe code) into spare
//!   capacity, then setting the fields that differ from call to call with
//!   values of the same types read from the function object and the
//!   registers. The copy takes the reference to the code block the image
//!   holds by adding one to its count, at the word [`rc_word`] found, which
//!   a probe of a live `Rc` checked first. It pops a frame only when that
//!   count stays above zero, and leaves any other frame to the runtime.
//!   Machine code calls only code the table names, which the table keeps
//!   alive, with a `Ctx` it builds in its own stack frame and the run's
//!   `Shared`; a run without the environment arena or the global stamp
//!   such code may read makes no such call.
//! * Code compiled with a [`crate::EnvLayout`] reads and writes only
//!   binding slots of declarative environment records it reached from the
//!   running frame's own environment, each record's kind and each slot's
//!   index checked first. Code compiled with [`crate::CallLayout::globals`]
//!   reads a global binding only at the address a [`GlobalSite`] holds and
//!   only while the runtime's stamp is the one the site was filled at,
//!   which is the runtime's promise that the address is still the slot's.
//! * The registers compiled code holds outside the register file are
//!   read only through a [`FrameHandle`], only while the machine frame
//!   exists (between [`Runtime::frame_entered`] and
//!   [`Runtime::frame_left`], while machine code waits in a call out), and
//!   only at the point whose index machine code stored in the `Ctx` along
//!   with its spill area's address before the call; the code generator
//!   wrote every listed slot before that store. While Rust runs, nothing
//!   here holds a reference into a `Ctx` or the `Shared`: every access is
//!   through raw pointers derived from the ones `enter` made, so the
//!   handle's reads and the trampolines' writes never overlap a live
//!   reference.
//! * The layout probes ([`vec_layout_of`], [`rc_word`], [`option_tag`])
//!   read only initialised bytes of live values, and say `None` when a type
//!   is not laid out as compiled code would read it.
#![allow(unsafe_code)]

use std::any::Any;
use std::ffi::c_void;
use std::mem::{align_of, offset_of, size_of};
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::ptr;
use std::rc::Rc;
use std::sync::atomic::{AtomicU8, Ordering};

use agnijs_js_compiler::bytecode::IcSite;

use crate::calls::GlobalSite;
use crate::handoff::{DeoptReason, FrameState, Location, Point, PointKind, Points, SideExit};
use crate::{
    layout, CallView, ExecMemoryKind, Exit, HeapView, JitValue, Outcome, Resume, Runtime, VecLayout,
};

/// No point: compiled code has not stored one since it was entered (it
/// has not called out yet).
pub(crate) const NO_POINT: u32 = u32::MAX;

/// Exit codes machine code returns; anything below `EXIT_BASE` is a pc the
/// runtime reported that compiled code has no successor block for. Such a
/// pc is one of the returning frame's own block and says "the interpreter
/// goes on" to whoever called the code: [`enter`] reads it so, and the
/// call thunk's "left" path makes it [`EXIT_INTERP`] before the calling
/// frame's machine code sees it ([`left_code`]), since that code would
/// take a pc for one of its own.
pub(crate) const EXIT_BASE: u32 = 0xFFFF_FF00;
pub(crate) const EXIT_RETURN: u32 = EXIT_BASE;
pub(crate) const EXIT_FRAME: u32 = EXIT_BASE + 1;
pub(crate) const EXIT_FAULT: u32 = EXIT_BASE + 2;
pub(crate) const EXIT_DONE: u32 = EXIT_BASE + 3;
pub(crate) const EXIT_INTERP: u32 = EXIT_BASE + 4;
pub(crate) const EXIT_PANIC: u32 = EXIT_BASE + 5;

type SlowFn<V> = extern "C" fn(*mut Ctx<V>, u32) -> u32;
type PollFn<V> = extern "C" fn(*mut Ctx<V>, u32, u32) -> u32;
type ArithFn = extern "C" fn(u32, f64, f64) -> f64;

/// What code needs from the runtime to run, and what the runtime reads of
/// it while it runs: fixed when the code is generated, shared by the
/// [`crate::Compiled`] and the handles of its running frames, and found by
/// the trampolines through [`Ctx::info`].
pub(crate) struct CodeInfo {
    /// The registers the code addresses; a shorter window is a runtime bug.
    pub registers: usize,
    /// It was compiled with a [`crate::HeapLayout`]: it reads the heap, so
    /// the arena base must be live.
    pub heap: bool,
    /// The inline-cache sites it reads (0 when it reads none).
    pub ic: usize,
    /// It reads `this` (`LdaThis`) from the context.
    pub this: bool,
    /// It was compiled with a [`crate::CallLayout`]: it pushes and pops the
    /// runtime's frames itself, so the frame stack must be described.
    pub calls: bool,
    /// It reads environment records ([`crate::EnvLayout`]): the arena's
    /// base must be live.
    pub envs: bool,
    /// It was compiled with [`crate::CallLayout::globals`]: it, or a
    /// function it calls directly, reads the runtime's global stamp.
    pub stamp: bool,
    /// It keeps no register in machine state (no spill area, no guess):
    /// its frame needs no handle, so machine code may call it directly.
    pub direct: bool,
    /// The safe points, side exits and entry guards ([`crate::handoff`]).
    pub points: Points,
    /// One per `LdaGlobal` instruction, in pc order (empty unless the
    /// layout has [`crate::CallLayout::globals`]).
    pub globals: Box<[GlobalSite]>,
    /// The call thunk the code's call sites call, kept while the code is.
    pub _thunk: Option<Rc<ExecMemory>>,
}

/// The state of one run that every compiled frame of it shares
/// (TASK-101.3.3): one activation of [`enter`] and the frames its machine
/// code pushes by calling compiled functions directly. Machine code gets
/// its address as its second argument and reads it at the offsets in
/// [`Offsets`]. The fields after `direct_calls` are Rust's alone.
#[repr(C)]
pub(crate) struct Shared<V> {
    /// The `&mut R` given to [`enter`], type-erased.
    rt: *mut c_void,
    slow: SlowFn<V>,
    /// [`Runtime::call`]: a `Call` instruction.
    call: SlowFn<V>,
    /// [`Runtime::fast`]: a binding or `this` load or store.
    fast: SlowFn<V>,
    poll: PollFn<V>,
    /// [`Runtime::arith`]: number operators without an instruction of
    /// their own, called without leaving the frame (it sees no VM state).
    arith: ArithFn,
    /// The side-exit helper: materialises the frame at a point and leaves
    /// for the interpreter.
    deopt: SlowFn<V>,
    /// [`Runtime::returned`]: a function called directly returned (pc,
    /// whether machine code popped its frame).
    ret: PollFn<V>,
    /// A function called directly left without returning (the exit code):
    /// the caller's frame is materialised before it leaves too.
    left: SlowFn<V>,
    /// [`Runtime::global`]: an `LdaGlobal` its site did not serve (pc,
    /// global site).
    global: PollFn<V>,
    /// A copy of the VM's interrupt-poll counter.
    counter: u32,
    /// Non-zero while [`Runtime::safe_point_due`]: every back-edge polls,
    /// and no call is made directly.
    attend: u32,
    /// The runtime's prototype epoch ([`HeapView`]), refreshed with
    /// `objects`.
    proto_epoch: u32,
    /// Padding: `counter`, `attend` and `proto_epoch` are four bytes
    /// each.
    _pad: u32,
    /// [`Runtime::stack_floor`]: machine code calls a function directly
    /// only while the native stack pointer is above it. Fixed for the run.
    floor: usize,
    /// The object arena's base ([`HeapView`]; null without a heap),
    /// refreshed after every runtime call: the arena may reallocate under
    /// one.
    objects: *mut u8,
    /// The frame stack's vector, the register file's vector and the
    /// current realm ([`CallView`]), fixed for the run; null without one.
    frames: *mut u8,
    registers: *mut u8,
    realm: *mut u8,
    /// The table of functions machine code may call directly
    /// ([`CallView`]), fixed for the run.
    callees: *const u8,
    /// The environment arena's base ([`HeapView`]), refreshed with
    /// `objects`.
    envs: *mut u8,
    /// [`Runtime::global_stamp`], fixed for the run; null without one.
    global_stamp: *const u64,
    /// [`Runtime::shape_epoch`], refreshed after every runtime call.
    shape_epoch: u64,
    /// Calls machine code made directly in this run.
    direct_calls: u64,
    panic: Option<Box<dyn Any + Send>>,
}

/// One compiled frame's state, at the offsets in [`Offsets`]: machine
/// code's first argument. [`enter`] builds the one of the frame it runs;
/// machine code builds the one of a function it calls directly, in its own
/// stack frame, from the callee's entry in the runtime's table
/// ([`crate::CalleeTable`]).
#[repr(C)]
pub(crate) struct Ctx<V> {
    /// The register window (refreshed after every runtime call).
    regs: *mut V,
    /// The run this frame belongs to.
    shared: *mut Shared<V>,
    /// The block's inline-cache sites ([`Runtime::ic_table`]), fixed for
    /// the run.
    ic: *const IcSite,
    /// The code's [`CodeInfo`], which outlives the run.
    info: *const CodeInfo,
    /// In: the entry pc. Out, for `EXIT_RETURN`: the pc after `Return`.
    pc: u32,
    /// The [`Point`] machine code is at, stored before every call out and
    /// side exit ([`NO_POINT`] until the first).
    point: u32,
    /// The compiled frame's spill area, where the point's registers are.
    spill: *mut u8,
    /// The accumulator, whenever control is outside machine code.
    acc: V,
    /// The frame's `this` ([`Runtime::this_value`]), fixed for the run;
    /// the accumulator's value when the code has no `LdaThis`.
    this: V,
}

/// Field offsets of [`Ctx`] and (the `s_` ones) [`Shared`] for the code
/// generator.
pub(crate) struct Offsets {
    pub regs: i32,
    pub shared: i32,
    pub ic: i32,
    pub info: i32,
    pub pc: i32,
    pub point: i32,
    pub spill: i32,
    pub acc: i32,
    pub this: i32,
    /// `size_of::<Ctx<V>>()`.
    pub ctx_size: u32,
    pub s_slow: i32,
    pub s_call: i32,
    pub s_fast: i32,
    pub s_poll: i32,
    pub s_arith: i32,
    pub s_deopt: i32,
    pub s_ret: i32,
    pub s_left: i32,
    pub s_global: i32,
    pub s_callees: i32,
    pub s_envs: i32,
    pub s_global_stamp: i32,
    pub s_counter: i32,
    pub s_attend: i32,
    pub s_proto_epoch: i32,
    pub s_floor: i32,
    pub s_objects: i32,
    pub s_frames: i32,
    pub s_registers: i32,
    pub s_realm: i32,
    pub s_shape_epoch: i32,
    pub s_direct_calls: i32,
}

pub(crate) fn offsets<V>() -> Offsets {
    Offsets {
        regs: offset_of!(Ctx<V>, regs) as i32,
        shared: offset_of!(Ctx<V>, shared) as i32,
        ic: offset_of!(Ctx<V>, ic) as i32,
        info: offset_of!(Ctx<V>, info) as i32,
        pc: offset_of!(Ctx<V>, pc) as i32,
        point: offset_of!(Ctx<V>, point) as i32,
        spill: offset_of!(Ctx<V>, spill) as i32,
        acc: offset_of!(Ctx<V>, acc) as i32,
        this: offset_of!(Ctx<V>, this) as i32,
        ctx_size: size_of::<Ctx<V>>() as u32,
        s_slow: offset_of!(Shared<V>, slow) as i32,
        s_call: offset_of!(Shared<V>, call) as i32,
        s_fast: offset_of!(Shared<V>, fast) as i32,
        s_poll: offset_of!(Shared<V>, poll) as i32,
        s_arith: offset_of!(Shared<V>, arith) as i32,
        s_deopt: offset_of!(Shared<V>, deopt) as i32,
        s_ret: offset_of!(Shared<V>, ret) as i32,
        s_left: offset_of!(Shared<V>, left) as i32,
        s_global: offset_of!(Shared<V>, global) as i32,
        s_callees: offset_of!(Shared<V>, callees) as i32,
        s_envs: offset_of!(Shared<V>, envs) as i32,
        s_global_stamp: offset_of!(Shared<V>, global_stamp) as i32,
        s_counter: offset_of!(Shared<V>, counter) as i32,
        s_attend: offset_of!(Shared<V>, attend) as i32,
        s_proto_epoch: offset_of!(Shared<V>, proto_epoch) as i32,
        s_floor: offset_of!(Shared<V>, floor) as i32,
        s_objects: offset_of!(Shared<V>, objects) as i32,
        s_frames: offset_of!(Shared<V>, frames) as i32,
        s_registers: offset_of!(Shared<V>, registers) as i32,
        s_realm: offset_of!(Shared<V>, realm) as i32,
        s_shape_epoch: offset_of!(Shared<V>, shape_epoch) as i32,
        s_direct_calls: offset_of!(Shared<V>, direct_calls) as i32,
    }
}

// ---- frame handles ---------------------------------------------------------

/// The runtime's view of one compiled frame while its machine code waits
/// in a call out: the JIT's equivalent of an interpreter `Frame`, kept on
/// a list the runtime owns ([`Runtime::frame_entered`]) so that the
/// collector finds every compiled frame without scanning the native stack
/// (ADR 0019 J6). It reads the point machine code stored last and the
/// registers that point lists in the frame's spill area.
///
/// Only a frame [`enter`] runs has a handle. A function machine code calls
/// directly keeps no register in machine state ([`CodeInfo::direct`]), so
/// there is nothing a handle of it would list.
pub struct FrameHandle<V> {
    ctx: *const Ctx<V>,
    info: Rc<CodeInfo>,
}

#[cfg(test)]
impl CodeInfo {
    /// The info of code with these points and nothing else, for tests.
    pub(crate) fn of_points(points: Points) -> Rc<CodeInfo> {
        Rc::new(CodeInfo {
            registers: 0,
            heap: false,
            ic: 0,
            this: false,
            calls: false,
            envs: false,
            stamp: false,
            direct: false,
            points,
            globals: Box::new([]),
            _thunk: None,
        })
    }
}

#[cfg(test)]
impl<V: JitValue> FrameHandle<V> {
    /// A handle over a context Rust built, for the tests of the reads
    /// above (which Miri can run: no machine code is involved).
    pub(crate) fn over(ctx: *const Ctx<V>, info: Rc<CodeInfo>) -> FrameHandle<V> {
        FrameHandle { ctx, info }
    }
}

#[cfg(test)]
impl<V: JitValue> Ctx<V> {
    /// A context as `enter` would build for a run that has stored `point`
    /// with its spill area at `spill`, for the tests of the handle.
    pub(crate) fn for_test(point: u32, spill: *mut u8, info: *const CodeInfo) -> Ctx<V> {
        Ctx {
            regs: ptr::null_mut(),
            shared: ptr::null_mut(),
            ic: ptr::null(),
            info,
            pc: 0,
            point,
            spill,
            acc: V::undefined(),
            this: V::undefined(),
        }
    }
}

// ---- vector and reference-count layout ----------------------------------------

/// See [`crate::vec_layout_of`].
pub(crate) fn vec_layout_of<T>() -> Option<VecLayout> {
    const WORD: usize = size_of::<usize>();
    if size_of::<T>() == 0 || size_of::<Vec<T>>() != 3 * WORD {
        return None;
    }
    let v: Vec<T> = Vec::with_capacity(7);
    let (ptr, cap) = (v.as_ptr() as usize, v.capacity());
    if cap == 0 || ptr == cap || ptr == 0 {
        return None;
    }
    // SAFETY: `Vec<T>` is three words (checked above) with no padding, all
    // of them initialised in a live vector, so reading it as `[usize; 3]`
    // reads three initialised words; the copy is not used as a vector.
    let words: [usize; 3] = unsafe { std::mem::transmute_copy(&v) };
    let at = |x: usize| -> Option<u32> {
        let mut found = None;
        for (i, &w) in words.iter().enumerate() {
            if w == x {
                if found.is_some() {
                    return None;
                }
                found = Some((i * WORD) as u32);
            }
        }
        found
    };
    // The length of an empty vector is the one word that is zero.
    Some(VecLayout {
        ptr: at(ptr)?,
        cap: at(cap)?,
        len: at(0)?,
    })
}

/// Whether an `Rc<T>` (of a sized `T`) is one word, the address of its
/// strong count: what compiled code relies on to take and give back a
/// reference ([`crate::CallLayout`]). Judged once, on a live `Rc`, by
/// watching the word at that address follow `Rc::strong_count`.
fn rc_is_counted_at_its_word() -> bool {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OK.get_or_init(|| {
        if size_of::<Rc<u64>>() != size_of::<usize>() {
            return false;
        }
        let a = Rc::new(0x5a5a_u64);
        // SAFETY: an `Rc<u64>` is one word (checked above), which is
        // copied here as a pointer and never used as an `Rc`.
        let word: *const usize = unsafe { std::mem::transmute_copy(&a) };
        if word.is_null() || !(word as usize).is_multiple_of(align_of::<usize>()) {
            return false;
        }
        // SAFETY: the word of a live `Rc` points into its allocation,
        // which starts with its two counts (`usize` each) and is at least
        // that large; the counts are read as plain words while no other
        // code runs. Should the standard library lay an `Rc` out some
        // other way, the comparisons below fail and no code is generated
        // that depends on it.
        let read = || unsafe { ptr::read(word) };
        let one = read() == 1 && Rc::strong_count(&a) == 1;
        let b = a.clone();
        let c = a.clone();
        let three = read() == 3 && Rc::strong_count(&a) == 3;
        drop(b);
        let two = read() == 2;
        drop(c);
        one && three && two && read() == 1
    })
}

/// See [`crate::option_tag`].
pub(crate) fn option_tag<T: Copy>(
    none: &Option<T>,
    some: &Option<T>,
    payload: usize,
) -> Option<(u32, u8)> {
    let inner = size_of::<T>();
    if inner != 4
        || size_of::<Option<T>>() != 2 * inner
        || !(payload == 0 || payload == inner)
        || none.is_some()
        || some.is_none()
    {
        return None;
    }
    // The payload fills one half, so the tag is in the other.
    let tag = if payload == 0 { inner } else { 0 };
    let byte = |o: &Option<T>| {
        // SAFETY: an option twice the size of its payload has no niche to
        // keep its discriminant in, so the discriminant is an integer in
        // the half the payload does not occupy, starting at that half's
        // first byte (it is no more aligned than the payload, and no wider
        // than the half); that byte is initialised in every value.
        unsafe { *(o as *const Option<T> as *const u8).add(tag) }
    };
    let (n, s) = (byte(none), byte(some));
    (n != s).then_some((tag as u32, s))
}

/// See [`crate::rc_word`].
pub(crate) fn rc_word<T>(rc: &Rc<T>) -> Option<*const usize> {
    if !rc_is_counted_at_its_word() || size_of::<Rc<T>>() != size_of::<usize>() {
        return None;
    }
    // SAFETY: an `Rc<T>` of a sized `T` is one word (checked above); it is
    // copied as a pointer and never used as an `Rc`.
    let word: *const usize = unsafe { std::mem::transmute_copy(rc) };
    Some(word)
}

/// See [`crate::vec_layout`].
pub(crate) fn vec_layout<T: Copy>(sample: T) -> Option<VecLayout> {
    const WORD: usize = size_of::<usize>();
    if size_of::<T>() == 0 || size_of::<Vec<T>>() != 3 * WORD {
        return None;
    }
    let mut v: Vec<T> = Vec::with_capacity(7);
    v.extend([sample; 3]);
    let (ptr, cap, len) = (v.as_ptr() as usize, v.capacity(), v.len());
    if cap == len || ptr == cap || ptr == len {
        return None;
    }
    // SAFETY: `Vec<T>` is three words (checked above) with no padding, all
    // of them initialised in a live vector, so reading it as `[usize; 3]`
    // reads three initialised words; the copy is not used as a vector.
    let words: [usize; 3] = unsafe { std::mem::transmute_copy(&v) };
    let at = |x: usize| -> Option<u32> {
        let mut found = None;
        for (i, &w) in words.iter().enumerate() {
            if w == x {
                if found.is_some() {
                    return None;
                }
                found = Some((i * WORD) as u32);
            }
        }
        found
    };
    Some(VecLayout {
        ptr: at(ptr)?,
        cap: at(cap)?,
        len: at(len)?,
    })
}

// ---- layout check ----------------------------------------------------------

fn tag_of<V: JitValue>(v: &V) -> u8 {
    // SAFETY: `layout_matches` calls this only once `V` is 16 bytes with
    // alignment 8; every value it passes has its tag byte at offset 0
    // initialised if `V` follows the layout, and a `V` that does not is
    // rejected by the comparison (the read is of a byte inside the value).
    unsafe { *(v as *const V as *const u8).add(layout::TAG_OFFSET) }
}

fn payload_u64<V: JitValue>(v: &V) -> u64 {
    // SAFETY: as above, for the eight payload bytes of a number, which a
    // value of the layout initialises.
    unsafe { std::ptr::read((v as *const V as *const u8).add(layout::PAYLOAD_OFFSET) as *const u64) }
}

fn payload_u8<V: JitValue>(v: &V) -> u8 {
    // SAFETY: as above, for the one payload byte of a boolean.
    unsafe { *(v as *const V as *const u8).add(layout::PAYLOAD_OFFSET) }
}

/// Whether `V` has the layout compiled code assumes, judged by the values
/// its constructors produce (the ones compiled code writes itself).
pub(crate) fn layout_matches<V: JitValue>() -> bool {
    if size_of::<V>() != layout::SIZE || align_of::<V>() != 8 {
        return false;
    }
    let n = V::number(-2.25);
    tag_of(&V::undefined()) == layout::TAG_UNDEFINED
        && tag_of(&V::null()) == layout::TAG_NULL
        && tag_of(&V::empty()) == layout::TAG_EMPTY
        && tag_of(&V::boolean(true)) == layout::TAG_BOOL
        && payload_u8(&V::boolean(true)) == 1
        && payload_u8(&V::boolean(false)) == 0
        && tag_of(&n) == layout::TAG_NUMBER
        && payload_u64(&n) == (-2.25f64).to_bits()
}

// ---- executable memory -----------------------------------------------------

/// Machine code in its own pages, read-execute for its whole life.
pub(crate) struct ExecMemory {
    #[cfg_attr(not(jit_host), allow(dead_code))]
    ptr: *mut u8,
    #[cfg_attr(not(jit_host), allow(dead_code))]
    len: usize,
}

impl ExecMemory {
    /// Copy `code` into fresh pages and make them executable.
    #[cfg(jit_host)]
    pub(crate) fn new(code: &[u8]) -> Option<ExecMemory> {
        if code.is_empty() {
            return None;
        }
        let len = code.len();
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        if let Some(ptr) = macos::map_jit(code) {
            memory_kind_seen(ExecMemoryKind::MapJit);
            return Some(ExecMemory { ptr, len });
        }
        let ptr = sys::alloc_rw(len)?;
        // SAFETY: `ptr` is a fresh read-write mapping of at least `len`
        // bytes that nothing else refers to.
        unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), ptr, len) };
        if !sys::make_rx(ptr, len) {
            sys::free(ptr, len);
            return None;
        }
        sys::flush_icache(ptr, len);
        memory_kind_seen(ExecMemoryKind::Protect);
        Some(ExecMemory { ptr, len })
    }

    #[cfg(not(jit_host))]
    #[allow(dead_code)]
    pub(crate) fn new(_code: &[u8]) -> Option<ExecMemory> {
        None
    }

    /// Where the code is: what machine code calls
    /// ([`crate::CalleeTable`]).
    pub(crate) fn address(&self) -> usize {
        self.ptr as usize
    }
}

impl Drop for ExecMemory {
    fn drop(&mut self) {
        #[cfg(jit_host)]
        sys::free(self.ptr, self.len);
    }
}

/// 0: none yet; otherwise 1 + the [`ExecMemoryKind`] last used.
static MEMORY_KIND: AtomicU8 = AtomicU8::new(0);

#[cfg_attr(not(jit_host), allow(dead_code))]
fn memory_kind_seen(kind: ExecMemoryKind) {
    let k = match kind {
        ExecMemoryKind::Protect => 1,
        ExecMemoryKind::MapJit => 2,
    };
    MEMORY_KIND.store(k, Ordering::Relaxed);
}

pub(crate) fn memory_kind() -> Option<ExecMemoryKind> {
    match MEMORY_KIND.load(Ordering::Relaxed) {
        1 => Some(ExecMemoryKind::Protect),
        2 => Some(ExecMemoryKind::MapJit),
        _ => None,
    }
}

/// Apple silicon's JIT memory. A process with the hardened runtime may not
/// make ordinary pages executable; with the allow-jit entitlement it may
/// map `MAP_JIT` pages read-write-execute, which the hardware still keeps
/// W^X per thread: writable for a thread that lifted its JIT write
/// protection, executable for the others and for that thread once it puts
/// the protection back. The copy below runs with protection lifted on this
/// thread only, and restores it before anything executes. Elsewhere on
/// macOS (and when `MAP_JIT` is refused) the ordinary path is used.
#[cfg(all(jit_host, target_os = "macos", target_arch = "aarch64"))]
mod macos {
    use std::ffi::c_int;

    /// `<sys/mman.h>`.
    const MAP_JIT: c_int = 0x0800;

    extern "C" {
        fn pthread_jit_write_protect_supported_np() -> c_int;
        fn pthread_jit_write_protect_np(enabled: c_int);
    }

    pub(super) fn map_jit(code: &[u8]) -> Option<*mut u8> {
        let len = code.len();
        // SAFETY: a query with no arguments.
        let per_thread = unsafe { pthread_jit_write_protect_supported_np() } != 0;
        // SAFETY: a fresh private anonymous mapping.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_PRIVATE | libc::MAP_ANON | MAP_JIT,
                -1,
                0,
            )
        };
        if p == libc::MAP_FAILED {
            return None;
        }
        let ptr = p as *mut u8;
        if per_thread {
            // SAFETY: lifting and restoring this thread's JIT write
            // protection has no memory effects of its own; between the two
            // calls this thread writes only the fresh mapping, of at least
            // `len` bytes, and runs no code from it.
            unsafe {
                pthread_jit_write_protect_np(0);
                std::ptr::copy_nonoverlapping(code.as_ptr(), ptr, len);
                pthread_jit_write_protect_np(1);
            }
        } else {
            // Without per-thread write protection (Apple silicon under
            // virtualization, such as CI runners) a MAP_JIT mapping is
            // plainly writable: write it, then drop write access, so the
            // code is never writable and executable at once while it runs.
            // SAFETY: `ptr..ptr+len` is the fresh mapping above.
            let rx = unsafe {
                std::ptr::copy_nonoverlapping(code.as_ptr(), ptr, len);
                libc::mprotect(p, len, libc::PROT_READ | libc::PROT_EXEC) == 0
            };
            if !rx {
                // SAFETY: our own mapping, of `len` bytes, never run.
                unsafe { libc::munmap(p, len) };
                return None;
            }
        }
        super::sys::flush_icache(ptr, len);
        Some(ptr)
    }
}

#[cfg(all(jit_host, windows))]
mod sys {
    use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
    use windows_sys::Win32::System::Memory::{
        VirtualAlloc, VirtualFree, VirtualProtect, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE,
        PAGE_EXECUTE_READ, PAGE_READWRITE,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    pub(super) fn alloc_rw(len: usize) -> Option<*mut u8> {
        // SAFETY: a fresh private allocation; no existing memory is touched.
        let p = unsafe {
            VirtualAlloc(std::ptr::null(), len, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE)
        };
        (!p.is_null()).then_some(p as *mut u8)
    }

    pub(super) fn make_rx(ptr: *mut u8, len: usize) -> bool {
        let mut old = 0;
        // SAFETY: `ptr..ptr+len` is our own allocation from `alloc_rw`.
        unsafe { VirtualProtect(ptr as *const _, len, PAGE_EXECUTE_READ, &mut old) != 0 }
    }

    pub(super) fn flush_icache(ptr: *mut u8, len: usize) {
        // SAFETY: as above; flushing has no memory effects.
        unsafe { FlushInstructionCache(GetCurrentProcess(), ptr as *const _, len) };
    }

    pub(super) fn free(ptr: *mut u8, _len: usize) {
        // SAFETY: `ptr` came from `alloc_rw` and no code in it is running
        // (its owner, a `Compiled`, is being dropped outside any run).
        unsafe { VirtualFree(ptr as *mut _, 0, MEM_RELEASE) };
    }
}

#[cfg(all(jit_host, unix))]
mod sys {
    pub(super) fn alloc_rw(len: usize) -> Option<*mut u8> {
        // SAFETY: a fresh private anonymous mapping.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        (p != libc::MAP_FAILED).then_some(p as *mut u8)
    }

    pub(super) fn make_rx(ptr: *mut u8, len: usize) -> bool {
        // SAFETY: `ptr..ptr+len` is our own page-aligned mapping.
        unsafe { libc::mprotect(ptr as *mut _, len, libc::PROT_READ | libc::PROT_EXEC) == 0 }
    }

    #[cfg(target_arch = "x86_64")]
    pub(super) fn flush_icache(_ptr: *mut u8, _len: usize) {
        // x86-64 keeps instruction fetch coherent with stores.
    }

    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    pub(super) fn flush_icache(ptr: *mut u8, len: usize) {
        extern "C" {
            fn sys_icache_invalidate(start: *mut std::ffi::c_void, len: usize);
        }
        // SAFETY: invalidates the instruction cache over our own mapping.
        unsafe { sys_icache_invalidate(ptr as *mut _, len) };
    }

    #[cfg(all(target_arch = "aarch64", not(target_os = "macos")))]
    pub(super) fn flush_icache(ptr: *mut u8, len: usize) {
        // The architectural sequence: clean the data cache to the point of
        // unification, invalidate the instruction cache, synchronise.
        let ctr: u64;
        // SAFETY: reading CTR_EL0 is permitted at EL0 on Linux and Android.
        unsafe { std::arch::asm!("mrs {}, ctr_el0", out(reg) ctr) };
        let dline = 4usize << ((ctr >> 16) & 0xf);
        let iline = 4usize << (ctr & 0xf);
        let start = ptr as usize;
        let end = start + len;
        let mut a = start & !(dline - 1);
        while a < end {
            // SAFETY: cache maintenance on our own mapping.
            unsafe { std::arch::asm!("dc cvau, {}", in(reg) a) };
            a += dline;
        }
        // SAFETY: barriers only.
        unsafe { std::arch::asm!("dsb ish") };
        let mut a = start & !(iline - 1);
        while a < end {
            // SAFETY: cache maintenance on our own mapping.
            unsafe { std::arch::asm!("ic ivau, {}", in(reg) a) };
            a += iline;
        }
        // SAFETY: barriers only.
        unsafe { std::arch::asm!("dsb ish", "isb") };
    }

    pub(super) fn free(ptr: *mut u8, len: usize) {
        // SAFETY: as in the Windows version.
        unsafe { libc::munmap(ptr as *mut _, len) };
    }
}

// ---- availability ------------------------------------------------------------

/// The start-up probe: generate a function returning 42, map it and call
/// it. Cached for the process.
pub(crate) fn available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(probe)
}

#[cfg(jit_host)]
fn probe() -> bool {
    let Ok(bytes) = crate::codegen::probe_code() else {
        return false;
    };
    let Some(mem) = ExecMemory::new(&bytes) else {
        return false;
    };
    // SAFETY: `mem` holds the code `probe_code` generated for exactly this
    // signature, made executable by `ExecMemory::new`.
    let f: extern "C" fn() -> u32 = unsafe { std::mem::transmute(mem.ptr) };
    f() == 42
}

#[cfg(not(jit_host))]
fn probe() -> bool {
    false
}

// ---- entry and trampolines ---------------------------------------------------

/// Run the machine code in `mem` from `pc`.
#[cfg_attr(not(jit_host), allow(unused_variables, unreachable_code))]
#[inline]
pub(crate) fn enter<V: JitValue, R: Runtime<V>>(
    mem: &ExecMemory,
    info: &Rc<CodeInfo>,
    rt: &mut R,
    pc: u32,
    acc: V,
) -> Outcome<V> {
    #[cfg(not(jit_host))]
    return Outcome {
        exit: Exit::Interp,
        pc,
        acc,
    };
    let interp = Outcome {
        exit: Exit::Interp,
        pc,
        acc,
    };
    let (ic, sites) = {
        let t = rt.ic_table();
        (t.as_ptr(), t.len())
    };
    if sites < info.ic {
        return interp;
    }
    // A function called directly from this code may read the heap even if
    // this code does not, so code that makes such calls takes the view too.
    let reads_heap = info.heap || info.calls;
    let view = if reads_heap {
        rt.heap()
    } else {
        HeapView::NONE
    };
    if reads_heap && view.objects.is_null() {
        return interp;
    }
    let calls = if info.calls {
        rt.call_view()
    } else {
        CallView::NONE
    };
    if info.calls
        && (calls.frames.is_null()
            || calls.registers.is_null()
            || calls.realm.is_null()
            || calls.callees.is_null())
    {
        return interp;
    }
    if info.envs && view.envs.is_null() {
        return interp;
    }
    let global_stamp = if info.stamp || info.calls {
        rt.global_stamp()
    } else {
        ptr::null()
    };
    if info.stamp && global_stamp.is_null() {
        return interp;
    }
    // A function this code calls directly may read global bindings and
    // environments even if this code does not: without either, no call is
    // made directly in this run (the stack pointer is never above such a
    // floor).
    let floor = if info.calls && (global_stamp.is_null() || view.envs.is_null()) {
        usize::MAX
    } else {
        rt.stack_floor()
    };
    let this = if info.this {
        match rt.this_value() {
            Some(t) => t,
            None => return interp,
        }
    } else {
        acc
    };
    let window = rt.registers();
    if window.len() < info.registers {
        return interp;
    }
    let regs = window.as_mut_ptr();
    let mut shared = Shared {
        rt: ptr::null_mut(),
        slow: slow_trampoline::<V, R>,
        call: call_trampoline::<V, R>,
        fast: fast_trampoline::<V, R>,
        poll: poll_trampoline::<V, R>,
        arith: arith_trampoline::<V, R>,
        deopt: deopt_trampoline::<V, R>,
        ret: ret_trampoline::<V, R>,
        left: left_trampoline::<V, R>,
        global: global_trampoline::<V, R>,
        counter: rt.interrupt_counter(),
        attend: rt.safe_point_due() as u32,
        proto_epoch: view.proto_epoch,
        _pad: 0,
        floor,
        objects: view.objects,
        frames: calls.frames,
        registers: calls.registers,
        realm: calls.realm,
        callees: calls.callees,
        envs: view.envs,
        global_stamp,
        shape_epoch: rt.shape_epoch(),
        direct_calls: 0,
        panic: None,
    };
    // From here until machine code has returned, `shared` and `ctx` are
    // used only through these pointers (the handle, the trampolines, the
    // reads below), so that no reference into either is live while another
    // path writes it.
    let shared_ptr: *mut Shared<V> = &mut shared;
    let mut ctx = Ctx {
        regs,
        shared: shared_ptr,
        ic,
        info: Rc::as_ptr(info),
        pc,
        point: NO_POINT,
        spill: ptr::null_mut(),
        acc,
        this,
    };
    let ctx_ptr: *mut Ctx<V> = &mut ctx;
    rt.frame_entered(FrameHandle {
        ctx: ctx_ptr,
        info: info.clone(),
    });
    let rt_ptr: *mut R = rt;
    // SAFETY: `shared` is a live local nothing else refers to.
    unsafe { (*shared_ptr).rt = rt_ptr as *mut c_void };
    // SAFETY: `mem` holds code generated by `codegen::compile` for this
    // `V` with the offsets of this `Ctx<V>` and `Shared<V>`, and the two
    // satisfy the module's obligations: they outlive the call, the window
    // is large enough, the inline-cache table has every site the code
    // reads and stays put while the block runs (the `Runtime::ic_table`
    // contract), the heap view is the runtime's current one
    // (`Runtime::heap`, the `HeapLayout` contract), the frame stack is the
    // runtime's own (`Runtime::call_view`, the `CallLayout` contract), the
    // info is the one the code was generated with (it outlives the run),
    // and `rt` is used only through `rt_ptr` until the call ends.
    let code: extern "C" fn(*mut Ctx<V>, *mut Shared<V>) -> u32 =
        unsafe { std::mem::transmute(mem.ptr) };
    let r = code(ctx_ptr, shared_ptr);
    // SAFETY: machine code has returned; `rt_ptr` is the live `&mut R`.
    let rt = unsafe { &mut *rt_ptr };
    // The machine frame is gone: the handle must not be read again.
    rt.frame_left();
    // SAFETY: machine code has returned and nothing refers to `shared` or
    // `ctx`.
    let (counter, direct, panic, pc, acc) = unsafe {
        (
            (*shared_ptr).counter,
            (*shared_ptr).direct_calls,
            (*shared_ptr).panic.take(),
            (*ctx_ptr).pc,
            (*ctx_ptr).acc,
        )
    };
    rt.set_interrupt_counter(counter);
    if direct != 0 {
        rt.direct_calls(direct);
    }
    if let Some(payload) = panic {
        resume_unwind(payload);
    }
    let exit = match r {
        EXIT_RETURN => Exit::Return,
        EXIT_FRAME => Exit::Frame,
        EXIT_FAULT => Exit::Fault,
        EXIT_DONE => Exit::Done,
        _ => Exit::Interp,
    };
    Outcome { exit, pc, acc }
}

// ---- the frame's state, read from the spill area ----------------------------

/// The registers point `id` of `points` lists, read from the spill area
/// of `ctx`: each a value compiled code stored whole. Nothing here makes a
/// value of bits (TASK-103.34): a register held unboxed is spilled with
/// the tag it has.
///
/// SAFETY: `ctx` must be the context of a machine frame that exists and
/// has stored `id` (or `id` is the point it is about to leave at), with no
/// reference into it live (module docs).
unsafe fn read_regs<V: JitValue>(ctx: *const Ctx<V>, points: &Points, id: u32) -> Vec<(u16, V)> {
    let Some(point) = points.at(id) else {
        return Vec::new();
    };
    if point.regs.is_empty() {
        return Vec::new();
    }
    let spill = (*ctx).spill;
    point
        .regs
        .iter()
        .map(|&(r, loc)| {
            let Location::Spill(off) = loc;
            // SAFETY: the code generator wrote a value of `V`'s layout at
            // the listed offset before storing the point: a tag word and a
            // payload word it wrote together as a value of that layout, or
            // loaded together from the register file.
            let v = ptr::read(spill.offset(off as isize) as *const V);
            (r, v)
        })
        .collect()
}

/// [`FrameState`] of the frame of `ctx` at point `id`.
///
/// SAFETY: as [`read_regs`].
unsafe fn state_at<V: JitValue>(
    ctx: *const Ctx<V>,
    points: &Points,
    id: u32,
    exit: Option<SideExit<V>>,
) -> FrameState<V> {
    FrameState {
        point: id,
        regs: read_regs(ctx, points, id),
        exit,
    }
}

impl<V: JitValue> FrameHandle<V> {
    /// The point the frame is at, if it has stored one.
    pub fn point(&self) -> Option<&Point> {
        // SAFETY: the handle is read only while it is on the runtime's
        // list (module docs), when the `Ctx` on `enter`'s stack is live and
        // no reference into it exists.
        let p = unsafe { (*self.ctx).point };
        self.info.points.at(p)
    }

    /// The bytecode pc of the point.
    pub fn pc(&self) -> Option<u32> {
        self.point().map(|p| p.pc)
    }

    /// Every value the frame holds outside the register file that may be
    /// a heap value, for the collector.
    pub fn live(&self, f: &mut dyn FnMut(V)) {
        // SAFETY: as in `point`.
        let id = unsafe { (*self.ctx).point };
        let Some(point) = self.info.points.at(id) else {
            return;
        };
        if point.regs.is_empty() {
            return;
        }
        // SAFETY: as in `point`.
        let spill = unsafe { (*self.ctx).spill };
        for &(_, loc) in &point.regs {
            let Location::Spill(off) = loc;
            // SAFETY: the code generator wrote a value of `V`'s layout at
            // the listed offset before storing the point.
            f(unsafe { ptr::read(spill.offset(off as isize) as *const V) });
        }
    }

    /// What to write into the interpreter frame now: the listed registers'
    /// values (and nothing to resume, since the frame is at a safe point
    /// whose instruction the runtime is running).
    pub fn state(&self) -> FrameState<V> {
        // SAFETY: as in `point`.
        unsafe {
            let id = (*self.ctx).point;
            state_at(self.ctx, &self.info.points, id, None)
        }
    }
}

fn exit_code(e: Exit) -> u32 {
    match e {
        Exit::Return => EXIT_RETURN,
        Exit::Frame => EXIT_FRAME,
        Exit::Fault => EXIT_FAULT,
        Exit::Done => EXIT_DONE,
        Exit::Interp => EXIT_INTERP,
    }
}

/// Run `f` against the runtime of `ctx` and translate its answer for
/// machine code. The code stored the point it calls from; when the runtime
/// leaves compiled code, or continues it at a pc the point has no
/// successor for, the frame is materialised first ([`Runtime::materialize`])
/// so the interpreter finds it complete.
fn call_out<V: JitValue, R: Runtime<V>>(
    ctx: *mut Ctx<V>,
    f: impl FnOnce(&mut R, V) -> Resume<V>,
) -> u32 {
    // SAFETY: machine code passes a `Ctx` of the run: the one `enter` made
    // or one it built itself for a direct call, with the `Shared` `enter`
    // made and the callee's `CodeInfo`, all live; nothing holds a reference
    // into them while machine code runs or waits here (module docs), so
    // they are read and written through the pointers.
    let (shared, rt_ptr, acc, counter, info, point) = unsafe {
        let c = ctx as *const Ctx<V>;
        let s = (*c).shared;
        (
            s,
            (*s).rt as *mut R,
            (*c).acc,
            (*s).counter,
            (*c).info,
            (*c).point,
        )
    };
    let r = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `shared.rt` is the `&mut R` given to `enter`, which uses
        // it only through this pointer until machine code returns.
        let rt = unsafe { &mut *rt_ptr };
        rt.set_interrupt_counter(counter);
        let resume = f(rt, acc);
        // SAFETY: the info outlives the run (the `Compiled` that owns the
        // code holds it, and whoever runs the code holds the `Compiled`).
        let info = unsafe { &*info };
        let points = &info.points;
        let resume = match resume {
            // Code that keeps nothing in machine state has nothing to
            // materialise: it leaves by itself for a pc it has no
            // successor for (an exit code below `EXIT_BASE`), its frame
            // being the runtime's already.
            Resume::Next { pc, acc } if !info.direct && !continues_at(points, point, pc) => {
                // SAFETY: as above; the runtime holds no reference into the
                // context either (it only ever sees handles).
                unsafe {
                    rt.materialize(state_at(ctx, points, point, None));
                    (*ctx).pc = pc;
                    (*ctx).acc = acc;
                }
                Resume::Exit(Exit::Interp)
            }
            Resume::Exit(e) => {
                // SAFETY: as above.
                unsafe { rt.materialize(state_at(ctx, points, point, None)) };
                Resume::Exit(e)
            }
            next => next,
        };
        let counter = rt.interrupt_counter();
        let attend = rt.safe_point_due() as u32;
        let window = match resume {
            Resume::Next { .. } => {
                let w = rt.registers();
                assert!(
                    w.len() >= info.registers,
                    "register window shrank under compiled code"
                );
                let w = w.as_mut_ptr();
                // The runtime may have allocated (moving the arena),
                // collected or changed a prototype: compiled code reads the
                // heap only through the view taken here.
                let reads_heap = info.heap || info.calls;
                let view = if reads_heap {
                    rt.heap()
                } else {
                    HeapView::NONE
                };
                assert!(
                    !reads_heap || !view.objects.is_null(),
                    "heap vanished under compiled code"
                );
                assert!(
                    !info.envs || !view.envs.is_null(),
                    "environments vanished under compiled code"
                );
                let epoch = if info.calls { rt.shape_epoch() } else { 0 };
                Some((w, view, epoch))
            }
            Resume::Exit(_) => None,
        };
        (resume, counter, attend, window)
    }));
    // SAFETY: as at the top: the runtime has returned, so nothing but this
    // function sees the contexts until machine code resumes.
    unsafe {
        match r {
            Ok((resume, counter, attend, window)) => {
                (*shared).counter = counter;
                (*shared).attend = attend;
                match resume {
                    Resume::Next { pc, acc } => {
                        (*ctx).acc = acc;
                        if let Some((w, view, epoch)) = window {
                            (*ctx).regs = w;
                            (*shared).objects = view.objects;
                            (*shared).envs = view.envs;
                            (*shared).proto_epoch = view.proto_epoch;
                            (*shared).shape_epoch = epoch;
                        }
                        pc
                    }
                    Resume::Exit(e) => exit_code(e),
                }
            }
            Err(payload) => {
                park_panic(shared, payload);
                EXIT_PANIC
            }
        }
    }
}

/// Keep a panic's payload for [`enter`] to resume once machine code has
/// returned (the first one, should unwinding the machine frames meet
/// another).
///
/// SAFETY: `shared` must be the live `Shared` of the run.
unsafe fn park_panic<V>(shared: *mut Shared<V>, payload: Box<dyn Any + Send>) {
    if (*shared).panic.is_none() {
        (*shared).panic = Some(payload);
    }
}

/// Whether compiled code at point `id` continues at `pc`.
fn continues_at(points: &Points, id: u32, pc: u32) -> bool {
    match points.at(id).map(|p| &p.kind) {
        Some(PointKind::SafePoint { next }) => next.contains(&pc),
        // Code compiled without points (none cached, nothing dirty):
        // whatever the runtime says, as the baseline did.
        None => true,
        Some(_) => false,
    }
}

/// The side-exit helper: compiled code at point `point` (an exit or an
/// entry guard) found a guard false. The frame is materialised with the
/// exit's pc and accumulator, and machine code returns [`EXIT_INTERP`].
extern "C" fn deopt_trampoline<V: JitValue, R: Runtime<V>>(ctx: *mut Ctx<V>, point: u32) -> u32 {
    // SAFETY: as in `call_out`.
    let shared = unsafe { (*ctx).shared };
    let r = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: as in `call_out`.
        let (rt_ptr, acc, points) =
            unsafe { ((*shared).rt as *mut R, (*ctx).acc, &(*(*ctx).info).points) };
        let p = points.at(point).unwrap_or_else(|| {
            panic!("compiled code left at a point it has no description of ({point})")
        });
        let guard = match p.kind {
            PointKind::Exit { guard } => guard,
            PointKind::Entry { guard } => Some(guard),
            PointKind::SafePoint { .. } => None,
        };
        // SAFETY: as in `call_out`.
        unsafe {
            (*ctx).point = point;
            (*ctx).pc = p.pc;
            let state = state_at(
                ctx,
                points,
                point,
                Some(SideExit {
                    pc: p.pc,
                    acc,
                    reason: DeoptReason::Guard,
                    guard,
                }),
            );
            (*rt_ptr).materialize(state);
        }
    }));
    match r {
        Ok(()) => EXIT_INTERP,
        Err(payload) => {
            // SAFETY: as in `call_out`.
            unsafe { park_panic(shared, payload) };
            EXIT_PANIC
        }
    }
}

extern "C" fn slow_trampoline<V: JitValue, R: Runtime<V>>(ctx: *mut Ctx<V>, pc: u32) -> u32 {
    call_out::<V, R>(ctx, |rt, acc| rt.slow(pc, acc))
}

extern "C" fn call_trampoline<V: JitValue, R: Runtime<V>>(ctx: *mut Ctx<V>, pc: u32) -> u32 {
    call_out::<V, R>(ctx, |rt, acc| rt.call(pc, acc))
}

/// A function machine code called directly returned to the `Call` at `pc`
/// and machine code did not finish the return itself: its frame is still
/// to pop (`popped` zero), or the poll after a return or a safe point is
/// due. The value it returned is the accumulator.
extern "C" fn ret_trampoline<V: JitValue, R: Runtime<V>>(
    ctx: *mut Ctx<V>,
    pc: u32,
    popped: u32,
) -> u32 {
    call_out::<V, R>(ctx, |rt, acc| rt.returned(pc, acc, popped != 0))
}

/// What the caller of a function machine code called directly leaves with
/// when that function answered `code` and did not return (TASK-103.36).
///
/// A word machine code answers means something to the frame that answers
/// it only: below [`EXIT_BASE`] it is a pc of that frame's own block. The
/// caller's call site compares what it is handed with its own `pc + 1`, so
/// a callee's pc must never reach it: a callee that left at the pc that
/// happens to be the caller's continuation would make the caller go on as
/// if the call had returned, the callee's frame still on the stack. So only
/// the exit codes that say the same to every frame pass ("a frame was
/// pushed", "a fault is pending", "the run is over", "the interpreter
/// goes on", "a panic is parked"). Anything else, a pc of the callee's
/// above all, is [`EXIT_INTERP`]: the callee's frame is the runtime's, at
/// the pc the runtime gave it, the caller's was written back for the call,
/// and the interpreter goes on from the two as they are.
fn left_code(code: u32) -> u32 {
    match code {
        EXIT_FRAME | EXIT_FAULT | EXIT_DONE | EXIT_INTERP | EXIT_PANIC => code,
        _ => EXIT_INTERP,
    }
}

/// A function machine code called directly left compiled code with the
/// word `code` instead of returning: the caller leaves too, with the exit
/// code that word is to it ([`left_code`]), its frame materialised first,
/// as [`call_out`] does for an exit of its own.
extern "C" fn left_trampoline<V: JitValue, R: Runtime<V>>(ctx: *mut Ctx<V>, code: u32) -> u32 {
    if code == EXIT_PANIC {
        return code;
    }
    // SAFETY: as in `call_out`.
    let shared = unsafe { (*ctx).shared };
    let r = catch_unwind(AssertUnwindSafe(|| {
        // The call thunk takes a return itself: it never hands one here.
        debug_assert_ne!(code, EXIT_RETURN, "a return is not a way of leaving");
        // SAFETY: as in `call_out`.
        unsafe {
            let rt = &mut *((*shared).rt as *mut R);
            let points = &(*(*ctx).info).points;
            rt.materialize(state_at(ctx, points, (*ctx).point, None));
        }
    }));
    match r {
        Ok(()) => left_code(code),
        Err(payload) => {
            // SAFETY: as in `call_out`.
            unsafe { park_panic(shared, payload) };
            EXIT_PANIC
        }
    }
}

/// Run `hit`, a runtime entry that serves the instruction at `pc` without
/// changing anything compiled code caches ([`Runtime::fast`],
/// [`Runtime::global`]), and answer machine code as [`call_out`] does, in
/// its three ways:
///
/// - `Some`: the instruction is done and the value is the accumulator. No
///   allocation, no poll, no safe point happened, so nothing is refreshed
///   and compiled code continues at `pc + 1`.
/// - `None`: the instruction runs by its generic semantics
///   ([`Runtime::slow`]), a call out like any other.
/// - A panic: the payload is parked for [`enter`] to resume and the answer
///   is [`EXIT_PANIC`] (TASK-103.35). It used to be dropped and the
///   instruction run again by the generic path, on whatever state the
///   panic had left, so that the host never learned there had been one.
fn hit_or_out<V: JitValue, R: Runtime<V>>(
    ctx: *mut Ctx<V>,
    pc: u32,
    hit: impl FnOnce(&mut R, V) -> Option<V>,
) -> u32 {
    // SAFETY: as in `call_out`.
    let (shared, rt_ptr, acc) = unsafe {
        let s = (*ctx).shared;
        (s, (*s).rt as *mut R, (*ctx).acc)
    };
    let r = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: as in `call_out`. The reference ends with this closure,
        // before the generic path below makes its own.
        let rt = unsafe { &mut *rt_ptr };
        hit(rt, acc)
    }));
    match r {
        Ok(Some(v)) => {
            // SAFETY: as in `call_out`.
            unsafe { (*ctx).acc = v };
            pc + 1
        }
        Ok(None) => call_out::<V, R>(ctx, |rt, acc| rt.slow(pc, acc)),
        Err(payload) => {
            // SAFETY: as in `call_out`.
            unsafe { park_panic(shared, payload) };
            EXIT_PANIC
        }
    }
}

extern "C" fn fast_trampoline<V: JitValue, R: Runtime<V>>(ctx: *mut Ctx<V>, pc: u32) -> u32 {
    hit_or_out::<V, R>(ctx, pc, |rt, acc| rt.fast(pc, acc))
}

/// An `LdaGlobal` at `pc` that its site (`site`) did not serve: the
/// runtime's cached read ([`Runtime::global`]), which may fill the site,
/// else the instruction by its generic semantics.
extern "C" fn global_trampoline<V: JitValue, R: Runtime<V>>(
    ctx: *mut Ctx<V>,
    pc: u32,
    site: u32,
) -> u32 {
    hit_or_out::<V, R>(ctx, pc, |rt, acc| {
        // SAFETY: the info of the code that is running, which outlives the
        // run (`call_out`); the site is one of its own.
        let info: &CodeInfo = unsafe { &*(*ctx).info };
        rt.global(pc, acc, info.globals.get(site as usize))
    })
}

extern "C" fn poll_trampoline<V: JitValue, R: Runtime<V>>(
    ctx: *mut Ctx<V>,
    pc: u32,
    target: u32,
) -> u32 {
    call_out::<V, R>(ctx, |rt, acc| rt.poll(pc, target, acc))
}

extern "C" fn arith_trampoline<V: JitValue, R: Runtime<V>>(op: u32, a: f64, b: f64) -> f64 {
    let op = match op {
        0 => crate::Arith::Mod,
        _ => crate::Arith::Exp,
    };
    // `Runtime::arith` is pure number arithmetic and documented not to
    // panic; should one implementation do so anyway, the panic must not
    // cross machine code, and the result is NaN.
    catch_unwind(|| R::arith(op, a, b)).unwrap_or(f64::NAN)
}

#[cfg(test)]
mod tests {
    //! The handle's reads over a context and spill area Rust built: no
    //! machine code runs, so Miri can check them (docs/miri-local.md).

    use super::*;

    #[repr(C, u8)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    #[allow(dead_code)]
    enum Val {
        Undefined = 0,
        Null = 1,
        Bool(bool) = 2,
        Number(f64) = 3,
        Obj(u32) = 7,
        Empty = 8,
    }

    impl JitValue for Val {
        fn undefined() -> Self {
            Val::Undefined
        }
        fn null() -> Self {
            Val::Null
        }
        fn boolean(b: bool) -> Self {
            Val::Bool(b)
        }
        fn number(n: f64) -> Self {
            Val::Number(n)
        }
        fn empty() -> Self {
            Val::Empty
        }
    }

    fn points() -> Rc<CodeInfo> {
        CodeInfo::of_points(Points {
            points: vec![
                Point {
                    pc: 3,
                    kind: PointKind::SafePoint { next: vec![4] },
                    regs: vec![
                        (2, Location::Spill(0)),
                        (5, Location::Spill(16)),
                        (6, Location::Spill(32)),
                    ],
                },
                Point {
                    pc: 7,
                    kind: PointKind::Exit { guard: Some(5) },
                    regs: vec![(5, Location::Spill(16))],
                },
            ],
        })
    }

    /// A spill area as compiled code fills it, a value per slot: an object
    /// at 0; at 16 a register held unboxed as a number, spilled under a
    /// number's tag; at 32 another such register that compiled code took
    /// without a guard and has not written, spilled as what the register
    /// file held (TASK-103.34). Typed fields rather than a byte buffer: a
    /// `Val` has padding, and a byte array may not hold uninitialised bytes.
    #[repr(C, align(16))]
    struct Spill {
        obj: Val,
        number: Val,
        unguarded: Val,
    }

    fn spill() -> Spill {
        const _: () = assert!(std::mem::size_of::<Val>() == 16);
        Spill {
            obj: Val::Obj(42),
            number: Val::Number(2.5),
            unguarded: Val::Undefined,
        }
    }

    #[test]
    fn a_handle_reads_the_registers_its_point_lists() {
        let info = points();
        let mut spill = spill();
        let ctx = Ctx::<Val>::for_test(0, &mut spill as *mut Spill as *mut u8, Rc::as_ptr(&info));
        let h = FrameHandle::over(&ctx, info.clone());
        assert_eq!(h.pc(), Some(3));
        let mut live = Vec::new();
        h.live(&mut |v| live.push(v));
        // Every listed value is offered to the collector, which knows a
        // heap value when it sees one: the code generator cannot say which
        // slot holds one.
        assert_eq!(live, vec![Val::Obj(42), Val::Number(2.5), Val::Undefined]);
        let st = h.state();
        assert_eq!(st.point, 0);
        assert_eq!(
            st.regs,
            vec![
                (2, Val::Obj(42)),
                (5, Val::Number(2.5)),
                (6, Val::Undefined)
            ],
            "each as the value it is: nothing is boxed from bits"
        );
        assert_eq!(st.exit, None);
    }

    #[test]
    fn a_handle_before_the_first_point_holds_nothing() {
        let info = points();
        let ctx = Ctx::<Val>::for_test(NO_POINT, ptr::null_mut(), Rc::as_ptr(&info));
        let h = FrameHandle::over(&ctx, info.clone());
        assert_eq!(h.pc(), None);
        let mut n = 0;
        h.live(&mut |_| n += 1);
        assert_eq!(n, 0);
        assert_eq!(h.state().regs, vec![]);
    }

    #[test]
    fn a_point_continues_only_at_its_successors() {
        let info = points();
        let points = &info.points;
        assert!(continues_at(points, 0, 4));
        assert!(!continues_at(points, 0, 5));
        assert!(!continues_at(points, 1, 8), "an exit continues nowhere");
        assert!(
            continues_at(points, NO_POINT, 9),
            "no point: as the baseline"
        );
    }

    #[test]
    fn a_callees_word_reaches_its_caller_as_an_exit_code_only() {
        // The exits that mean the same to every frame pass.
        for code in [EXIT_FRAME, EXIT_FAULT, EXIT_DONE, EXIT_INTERP, EXIT_PANIC] {
            assert_eq!(left_code(code), code);
        }
        // A pc is the callee's own: never the caller's to continue at.
        for pc in [0, 1, 7, 20_000, EXIT_BASE - 1] {
            assert_eq!(left_code(pc), EXIT_INTERP, "pc {pc}");
        }
        // Nor does a word no exit is defined for pass, or a return (which
        // the thunk never hands over).
        assert_eq!(left_code(EXIT_PANIC + 1), EXIT_INTERP);
        assert_eq!(left_code(u32::MAX), EXIT_INTERP);
        assert_eq!(left_code(EXIT_RETURN), EXIT_INTERP);
    }

    /// The words compiled code relies on to push a frame (TASK-101.3.3):
    /// an `Rc` is the address of its strong count, and a vector's three
    /// words are found on an empty one.
    #[test]
    fn an_rc_is_the_address_of_its_strong_count() {
        assert!(rc_is_counted_at_its_word());
        let a = Rc::new([7u64; 3]);
        let word = rc_word(&a).expect("one word");
        let b = a.clone();
        // SAFETY: the word of a live `Rc` is the address of its strong
        // count (the probe above), read while nothing else runs.
        let strong = unsafe { ptr::read(word) };
        assert_eq!(strong, 2);
        assert_eq!(rc_word(&b), Some(word), "clones are the same word");
        drop(b);
        // SAFETY: as above.
        let strong = unsafe { ptr::read(word) };
        assert_eq!(strong, 1);
    }

    #[test]
    fn an_option_of_an_id_shows_its_tag() {
        #[derive(Clone, Copy)]
        struct Id(#[allow(dead_code)] u32);
        let some = Some(Id(7));
        let payload = match &some {
            Some(id) => id as *const Id as usize - &some as *const Option<Id> as usize,
            None => unreachable!(),
        };
        let (tag, s) = option_tag(&None::<Id>, &some, payload).expect("two words");
        assert!(tag == 0 || tag == 4);
        assert_ne!(tag as usize, payload);
        let (_, again) = option_tag(&None::<Id>, &Some(Id(u32::MAX)), payload).unwrap();
        assert_eq!(s, again, "the tag does not depend on the id");
        assert_eq!(option_tag(&Some(Id(1)), &some, payload), None, "not a None");
        // An option with a niche is one word: no tag to read.
        let nz = std::num::NonZeroU32::new(5);
        assert_eq!(option_tag(&None, &nz, 0), None);
    }

    #[test]
    fn a_vector_layout_is_found_without_a_sample() {
        struct NotCopy {
            _a: String,
            _b: u64,
        }
        let l = vec_layout_of::<NotCopy>().expect("three words");
        assert_eq!(
            Some(l),
            vec_layout_of::<u64>(),
            "one layout for every element type"
        );
        assert_eq!(
            Some(l),
            vec_layout(0u64),
            "and the one a filled vector shows"
        );
        let mut all = [l.ptr, l.cap, l.len];
        all.sort_unstable();
        let w = size_of::<usize>() as u32;
        assert_eq!(all, [0, w, 2 * w]);
        assert_eq!(
            vec_layout_of::<()>(),
            None,
            "no layout for a zero-sized element"
        );
    }
}
