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
//! * Machine code is only entered through [`enter`], with a [`Ctx`] that
//!   lives on `enter`'s stack frame for the whole run, whose register
//!   window has at least as many registers as the code addresses, and
//!   whose runtime pointer is the `&mut R` `enter` was given, used only
//!   through that pointer until the run ends.
//! * Machine code writes values only with the tags and payloads of
//!   [`crate::layout`], which [`layout_matches`] checks against the value
//!   type's own constructors before any code for that type is generated.
//! * Code compiled with a [`crate::HeapLayout`] reads the heap only
//!   through the arena base and prototype epoch in the `Ctx`, which are the
//!   runtime's own ([`Runtime::heap`]) on entry and after every runtime
//!   call; it keeps neither, nor any address or value derived from them,
//!   in a machine register across a call (the code generator reloads them
//!   from the `Ctx` after each). It reads inline caches from the table
//!   [`Runtime::ic_table`] gave on entry, which has every site it reads.
//! * No panic crosses machine code: each trampoline catches it, parks the
//!   payload in the `Ctx`, and returns `EXIT_PANIC`; `enter` resumes the
//!   unwind once machine code has returned.
//! * The registers compiled code holds outside the register file are
//!   read only through a [`FrameHandle`], only while the machine frame
//!   exists (between [`Runtime::frame_entered`] and
//!   [`Runtime::frame_left`], while machine code waits in a call out), and
//!   only at the point whose index machine code stored in the `Ctx` along
//!   with its spill area's address before the call; the code generator
//!   wrote every listed slot before that store. While Rust runs, nothing
//!   here holds a reference into the `Ctx`: every access is through raw
//!   pointers derived from the one `enter` made, so the handle's reads and
//!   the trampoline's writes never overlap a live reference.
#![allow(unsafe_code)]

use std::any::Any;
use std::ffi::c_void;
use std::mem::{align_of, offset_of, size_of};
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::ptr;
use std::rc::Rc;
use std::sync::atomic::{AtomicU8, Ordering};

use agnijs_js_compiler::bytecode::IcSite;

use crate::handoff::{DeoptReason, FrameState, Location, Point, PointKind, Points, SideExit};
use crate::{layout, ExecMemoryKind, Exit, HeapView, JitValue, Outcome, Resume, Runtime, VecLayout};

/// No point: compiled code has not stored one since it was entered (it
/// has not called out yet).
pub(crate) const NO_POINT: u32 = u32::MAX;

/// Exit codes machine code returns; anything below `EXIT_BASE` is a pc the
/// runtime reported that compiled code has no successor block for.
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

/// The state machine code runs against, at the offsets in [`Offsets`].
/// The fields after `acc` are Rust's alone.
#[repr(C)]
pub(crate) struct Ctx<V> {
    /// The register window (refreshed after every runtime call).
    regs: *mut V,
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
    /// In: the entry pc. Out, for `EXIT_RETURN`: the pc after `Return`.
    pc: u32,
    /// A copy of the VM's interrupt-poll counter.
    counter: u32,
    /// Non-zero while [`Runtime::safe_point_due`]: every back-edge polls.
    attend: u32,
    /// The runtime's prototype epoch ([`HeapView`]), refreshed with
    /// `objects`.
    proto_epoch: u32,
    /// The object arena's base ([`HeapView`]; null for code compiled
    /// without a [`crate::HeapLayout`]), refreshed after every runtime call:
    /// the arena may reallocate under one.
    objects: *mut u8,
    /// The block's inline-cache sites ([`Runtime::ic_table`]), fixed for the
    /// run.
    ic: *const IcSite,
    /// The side-exit helper: materialises the frame at a point and leaves
    /// for the interpreter.
    deopt: SlowFn<V>,
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
    /// Registers the code addresses; a shorter window is a runtime bug.
    registers: usize,
    /// Whether the code reads the heap (`objects` must be live).
    heap: bool,
    /// The code's points, which outlive the run (`enter` borrows them).
    points: *const Points,
    panic: Option<Box<dyn Any + Send>>,
}

/// Field offsets of [`Ctx`] for the code generator.
pub(crate) struct Offsets {
    pub regs: i32,
    pub this: i32,
    pub slow: i32,
    pub call: i32,
    pub fast: i32,
    pub poll: i32,
    pub arith: i32,
    pub pc: i32,
    pub counter: i32,
    pub attend: i32,
    pub proto_epoch: i32,
    pub objects: i32,
    pub ic: i32,
    pub deopt: i32,
    pub point: i32,
    pub spill: i32,
    pub acc: i32,
}

pub(crate) fn offsets<V>() -> Offsets {
    Offsets {
        regs: offset_of!(Ctx<V>, regs) as i32,
        this: offset_of!(Ctx<V>, this) as i32,
        slow: offset_of!(Ctx<V>, slow) as i32,
        call: offset_of!(Ctx<V>, call) as i32,
        fast: offset_of!(Ctx<V>, fast) as i32,
        poll: offset_of!(Ctx<V>, poll) as i32,
        arith: offset_of!(Ctx<V>, arith) as i32,
        pc: offset_of!(Ctx<V>, pc) as i32,
        counter: offset_of!(Ctx<V>, counter) as i32,
        attend: offset_of!(Ctx<V>, attend) as i32,
        proto_epoch: offset_of!(Ctx<V>, proto_epoch) as i32,
        objects: offset_of!(Ctx<V>, objects) as i32,
        ic: offset_of!(Ctx<V>, ic) as i32,
        deopt: offset_of!(Ctx<V>, deopt) as i32,
        point: offset_of!(Ctx<V>, point) as i32,
        spill: offset_of!(Ctx<V>, spill) as i32,
        acc: offset_of!(Ctx<V>, acc) as i32,
    }
}

// ---- frame handles ---------------------------------------------------------

/// The runtime's view of one compiled frame while its machine code waits
/// in a call out: the JIT's equivalent of an interpreter `Frame`, kept on
/// a list the runtime owns ([`Runtime::frame_entered`]) so that the
/// collector finds every compiled frame without scanning the native stack
/// (ADR 0019 J6). It reads the point machine code stored last and the
/// registers that point lists in the frame's spill area.
pub struct FrameHandle<V> {
    ctx: *const Ctx<V>,
    points: Rc<Points>,
}

#[cfg(test)]
impl<V: JitValue> FrameHandle<V> {
    /// A handle over a context Rust built, for the tests of the reads
    /// above (which Miri can run: no machine code is involved).
    pub(crate) fn over(ctx: *const Ctx<V>, points: Rc<Points>) -> FrameHandle<V> {
        FrameHandle { ctx, points }
    }
}

#[cfg(test)]
impl<V: JitValue> Ctx<V> {
    /// A context as `enter` would build for a run that has stored `point`
    /// with its spill area at `spill`, for the tests of the handle.
    pub(crate) fn for_test(point: u32, spill: *mut u8, points: *const Points) -> Ctx<V> {
        Ctx {
            regs: ptr::null_mut(),
            rt: ptr::null_mut(),
            slow: never_called::<V>,
            call: never_called::<V>,
            fast: never_called::<V>,
            poll: never_polled::<V>,
            arith: never_arith,
            pc: 0,
            counter: 0,
            attend: 0,
            proto_epoch: 0,
            objects: ptr::null_mut(),
            ic: ptr::null(),
            deopt: never_called::<V>,
            point,
            spill,
            acc: V::undefined(),
            this: V::undefined(),
            registers: 0,
            heap: false,
            points,
            panic: None,
        }
    }
}

#[cfg(test)]
extern "C" fn never_called<V>(_: *mut Ctx<V>, _: u32) -> u32 {
    unreachable!()
}
#[cfg(test)]
extern "C" fn never_polled<V>(_: *mut Ctx<V>, _: u32, _: u32) -> u32 {
    unreachable!()
}
#[cfg(test)]
extern "C" fn never_arith(_: u32, _: f64, _: f64) -> f64 {
    unreachable!()
}

/// What code needs from the runtime to be entered.
pub(crate) struct Needs {
    /// The registers it addresses.
    pub registers: usize,
    /// It was compiled with a [`crate::HeapLayout`].
    pub heap: bool,
    /// The inline-cache sites it reads (0 when it reads none).
    pub ic: usize,
    /// It reads `this` (`LdaThis`) from the context.
    pub this: bool,
}

// ---- vector layout ---------------------------------------------------------

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
    points: &Rc<Points>,
    needs: Needs,
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
    if sites < needs.ic {
        return interp;
    }
    let view = if needs.heap { rt.heap() } else { HeapView::NONE };
    if needs.heap && view.objects.is_null() {
        return interp;
    }
    let this = if needs.this {
        match rt.this_value() {
            Some(t) => t,
            None => return interp,
        }
    } else {
        acc
    };
    let window = rt.registers();
    if window.len() < needs.registers {
        return interp;
    }
    let regs = window.as_mut_ptr();
    let counter = rt.interrupt_counter();
    let attend = rt.safe_point_due() as u32;
    let mut ctx = Ctx {
        regs,
        rt: ptr::null_mut(),
        slow: slow_trampoline::<V, R>,
        call: call_trampoline::<V, R>,
        fast: fast_trampoline::<V, R>,
        poll: poll_trampoline::<V, R>,
        arith: arith_trampoline::<V, R>,
        pc,
        counter,
        attend,
        proto_epoch: view.proto_epoch,
        objects: view.objects,
        ic,
        deopt: deopt_trampoline::<V, R>,
        point: NO_POINT,
        spill: ptr::null_mut(),
        acc,
        this,
        registers: needs.registers,
        heap: needs.heap,
        points: Rc::as_ptr(points),
        panic: None,
    };
    // From here until machine code has returned, `ctx` is used only
    // through this pointer (the handle, the trampolines, the reads below),
    // so that no reference into it is live while another path writes it.
    let ctx_ptr: *mut Ctx<V> = &mut ctx;
    rt.frame_entered(FrameHandle {
        ctx: ctx_ptr,
        points: points.clone(),
    });
    let rt_ptr: *mut R = rt;
    // SAFETY: `ctx` is a live local nothing else refers to.
    unsafe { (*ctx_ptr).rt = rt_ptr as *mut c_void };
    // SAFETY: `mem` holds code generated by `codegen::compile` for this
    // `V` with the offsets of this `Ctx<V>`, and `ctx` satisfies the
    // module's obligations: it outlives the call, its window is large
    // enough, its inline-cache table has every site the code reads and
    // stays put while the block runs (the `Runtime::ic_table` contract),
    // its heap view is the runtime's current one (`Runtime::heap`, the
    // `HeapLayout` contract), its points are the ones the code was
    // generated with (they outlive the run), and `rt` is used only through
    // `rt_ptr` until the call ends.
    let code: extern "C" fn(*mut Ctx<V>) -> u32 = unsafe { std::mem::transmute(mem.ptr) };
    let r = code(ctx_ptr);
    // SAFETY: machine code has returned; `rt_ptr` is the live `&mut R`.
    let rt = unsafe { &mut *rt_ptr };
    // The machine frame is gone: the handle must not be read again.
    rt.frame_left();
    // SAFETY: machine code has returned and nothing refers to `ctx`.
    let (counter, panic, pc, acc) =
        unsafe { ((*ctx_ptr).counter, (*ctx_ptr).panic.take(), (*ctx_ptr).pc, (*ctx_ptr).acc) };
    rt.set_interrupt_counter(counter);
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
/// of `ctx` (boxed values whole, numbers boxed by `V::number`).
///
/// SAFETY: `ctx` must be the context of a machine frame that exists and
/// has stored `id` (or `id` is the point it is about to leave at), with no
/// reference into it live (module docs).
unsafe fn read_regs<V: JitValue>(ctx: *const Ctx<V>, points: &Points, id: u32) -> Vec<(u16, V)> {
    let Some(point) = points.at(id) else {
        return Vec::new();
    };
    let spill = (*ctx).spill;
    point
        .regs
        .iter()
        .map(|&(r, loc)| {
            let v = match loc {
                // SAFETY: the code generator wrote a value of `V`'s layout
                // at the listed offset before storing the point.
                Location::Spill(off) => ptr::read(spill.offset(off as isize) as *const V),
                Location::SpillNumber(off) => {
                    V::number(f64::from_bits(ptr::read(spill.offset(off as isize) as *const u64)))
                }
            };
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
        self.points.at(p)
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
        let Some(point) = self.points.at(id) else { return };
        // SAFETY: as in `point`.
        let spill = unsafe { (*self.ctx).spill };
        for &(_, loc) in &point.regs {
            if let Location::Spill(off) = loc {
                // SAFETY: the code generator wrote a value of `V`'s layout
                // at the listed offset before storing the point.
                f(unsafe { ptr::read(spill.offset(off as isize) as *const V) });
            }
        }
    }

    /// What to write into the interpreter frame now: the listed registers'
    /// values (and nothing to resume, since the frame is at a safe point
    /// whose instruction the runtime is running).
    pub fn state(&self) -> FrameState<V> {
        // SAFETY: as in `point`.
        unsafe {
            let id = (*self.ctx).point;
            state_at(self.ctx, &self.points, id, None)
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
    // SAFETY: machine code passes the `Ctx` `enter` gave it, which is live;
    // nothing holds a reference into it while machine code runs or waits
    // here (module docs), so it is read and written through the pointer.
    let (rt_ptr, acc, counter, registers, heap, point, points) = unsafe {
        let c = ctx as *const Ctx<V>;
        (
            (*c).rt as *mut R,
            (*c).acc,
            (*c).counter,
            (*c).registers,
            (*c).heap,
            (*c).point,
            (*c).points,
        )
    };
    let r = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `ctx.rt` is the `&mut R` given to `enter`, which uses it
        // only through this pointer until machine code returns.
        let rt = unsafe { &mut *rt_ptr };
        rt.set_interrupt_counter(counter);
        let resume = f(rt, acc);
        // SAFETY: the points outlive the run (`enter` borrows them).
        let points = unsafe { &*points };
        let resume = match resume {
            Resume::Next { pc, acc } if !continues_at(points, point, pc) => {
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
                assert!(w.len() >= registers, "register window shrank under compiled code");
                let w = w.as_mut_ptr();
                // The runtime may have allocated (moving the arena),
                // collected or changed a prototype: compiled code reads the
                // heap only through the view taken here.
                let view = if heap { rt.heap() } else { HeapView::NONE };
                assert!(!heap || !view.objects.is_null(), "heap vanished under compiled code");
                Some((w, view))
            }
            Resume::Exit(_) => None,
        };
        (resume, counter, attend, window)
    }));
    // SAFETY: as at the top: the runtime has returned, so nothing but this
    // function sees the context until machine code resumes.
    unsafe {
        match r {
            Ok((resume, counter, attend, window)) => {
                (*ctx).counter = counter;
                (*ctx).attend = attend;
                match resume {
                    Resume::Next { pc, acc } => {
                        (*ctx).acc = acc;
                        if let Some((w, view)) = window {
                            (*ctx).regs = w;
                            (*ctx).objects = view.objects;
                            (*ctx).proto_epoch = view.proto_epoch;
                        }
                        pc
                    }
                    Resume::Exit(e) => exit_code(e),
                }
            }
            Err(payload) => {
                (*ctx).panic = Some(payload);
                EXIT_PANIC
            }
        }
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
    let r = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: as in `call_out`.
        let (rt_ptr, acc, points) = unsafe { ((*ctx).rt as *mut R, (*ctx).acc, &*(*ctx).points) };
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
            unsafe { (*ctx).panic = Some(payload) };
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

extern "C" fn fast_trampoline<V: JitValue, R: Runtime<V>>(ctx: *mut Ctx<V>, pc: u32) -> u32 {
    {
        // SAFETY: as in `call_out`.
        let (rt_ptr, acc) = unsafe { ((*ctx).rt as *mut R, (*ctx).acc) };
        // SAFETY: as in `call_out`.
        let rt = unsafe { &mut *rt_ptr };
        // A hit changes nothing compiled code caches: no allocation, no
        // poll, no safe point (`Runtime::fast`).
        if let Ok(Some(v)) = catch_unwind(AssertUnwindSafe(|| rt.fast(pc, acc))) {
            // SAFETY: as in `call_out`.
            unsafe { (*ctx).acc = v };
            return pc + 1;
        }
    }
    call_out::<V, R>(ctx, |rt, acc| rt.slow(pc, acc))
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

    fn points() -> Rc<Points> {
        Rc::new(Points {
            points: vec![
                Point {
                    pc: 3,
                    kind: PointKind::SafePoint { next: vec![4] },
                    regs: vec![(2, Location::Spill(0)), (5, Location::SpillNumber(16))],
                },
                Point {
                    pc: 7,
                    kind: PointKind::Exit { guard: Some(5) },
                    regs: vec![(5, Location::SpillNumber(16))],
                },
            ],
        })
    }

    /// A spill area as compiled code fills it: a boxed object at 0, the
    /// bits of a number at 16. Typed fields rather than a byte buffer: a
    /// `Val` has padding, and a byte array may not hold uninitialised bytes.
    #[repr(C, align(16))]
    struct Spill {
        obj: Val,
        number_bits: u64,
    }

    fn spill() -> Spill {
        const _: () = assert!(std::mem::size_of::<Val>() == 16);
        Spill { obj: Val::Obj(42), number_bits: 2.5f64.to_bits() }
    }

    #[test]
    fn a_handle_reads_the_registers_its_point_lists() {
        let points = points();
        let mut spill = spill();
        let ctx =
            Ctx::<Val>::for_test(0, &mut spill as *mut Spill as *mut u8, Rc::as_ptr(&points));
        let h = FrameHandle::over(&ctx, points.clone());
        assert_eq!(h.pc(), Some(3));
        let mut live = Vec::new();
        h.live(&mut |v| live.push(v));
        assert_eq!(live, vec![Val::Obj(42)], "numbers are never heap values");
        let st = h.state();
        assert_eq!(st.point, 0);
        assert_eq!(st.regs, vec![(2, Val::Obj(42)), (5, Val::Number(2.5))]);
        assert_eq!(st.exit, None);
    }

    #[test]
    fn a_handle_before_the_first_point_holds_nothing() {
        let points = points();
        let ctx = Ctx::<Val>::for_test(NO_POINT, ptr::null_mut(), Rc::as_ptr(&points));
        let h = FrameHandle::over(&ctx, points.clone());
        assert_eq!(h.pc(), None);
        let mut n = 0;
        h.live(&mut |_| n += 1);
        assert_eq!(n, 0);
        assert_eq!(h.state().regs, vec![]);
    }

    #[test]
    fn a_point_continues_only_at_its_successors() {
        let points = points();
        assert!(continues_at(&points, 0, 4));
        assert!(!continues_at(&points, 0, 5));
        assert!(!continues_at(&points, 1, 8), "an exit continues nowhere");
        assert!(continues_at(&points, NO_POINT, 9), "no point: as the baseline");
    }
}
