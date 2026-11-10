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
#![allow(unsafe_code)]

use std::any::Any;
use std::ffi::c_void;
use std::mem::{align_of, offset_of, size_of};
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU8, Ordering};

use agnijs_js_compiler::bytecode::IcSite;

use crate::{layout, ExecMemoryKind, Exit, HeapView, JitValue, Outcome, Resume, Runtime, VecLayout};

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
    /// The accumulator, whenever control is outside machine code.
    acc: V,
    /// Registers the code addresses; a shorter window is a runtime bug.
    registers: usize,
    /// Whether the code reads the heap (`objects` must be live).
    heap: bool,
    panic: Option<Box<dyn Any + Send>>,
}

/// Field offsets of [`Ctx`] for the code generator.
pub(crate) struct Offsets {
    pub regs: i32,
    pub slow: i32,
    pub poll: i32,
    pub arith: i32,
    pub pc: i32,
    pub counter: i32,
    pub attend: i32,
    pub proto_epoch: i32,
    pub objects: i32,
    pub ic: i32,
    pub acc: i32,
}

pub(crate) fn offsets<V>() -> Offsets {
    Offsets {
        regs: offset_of!(Ctx<V>, regs) as i32,
        slow: offset_of!(Ctx<V>, slow) as i32,
        poll: offset_of!(Ctx<V>, poll) as i32,
        arith: offset_of!(Ctx<V>, arith) as i32,
        pc: offset_of!(Ctx<V>, pc) as i32,
        counter: offset_of!(Ctx<V>, counter) as i32,
        attend: offset_of!(Ctx<V>, attend) as i32,
        proto_epoch: offset_of!(Ctx<V>, proto_epoch) as i32,
        objects: offset_of!(Ctx<V>, objects) as i32,
        ic: offset_of!(Ctx<V>, ic) as i32,
        acc: offset_of!(Ctx<V>, acc) as i32,
    }
}

/// What code needs from the runtime to be entered.
pub(crate) struct Needs {
    /// The registers it addresses.
    pub registers: usize,
    /// It was compiled with a [`crate::HeapLayout`].
    pub heap: bool,
    /// The inline-cache sites it reads (0 when it reads none).
    pub ic: usize,
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
pub(crate) fn enter<V: JitValue, R: Runtime<V>>(
    mem: &ExecMemory,
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
    let window = rt.registers();
    if window.len() < needs.registers {
        return interp;
    }
    let regs = window.as_mut_ptr();
    let counter = rt.interrupt_counter();
    let attend = rt.safe_point_due() as u32;
    let rt_ptr: *mut R = rt;
    let mut ctx = Ctx {
        regs,
        rt: rt_ptr as *mut c_void,
        slow: slow_trampoline::<V, R>,
        poll: poll_trampoline::<V, R>,
        arith: arith_trampoline::<V, R>,
        pc,
        counter,
        attend,
        proto_epoch: view.proto_epoch,
        objects: view.objects,
        ic,
        acc,
        registers: needs.registers,
        heap: needs.heap,
        panic: None,
    };
    // SAFETY: `mem` holds code generated by `codegen::compile` for this
    // `V` with the offsets of this `Ctx<V>`, and `ctx` satisfies the
    // module's obligations: it outlives the call, its window is large
    // enough, its inline-cache table has every site the code reads and
    // stays put while the block runs (the `Runtime::ic_table` contract),
    // its heap view is the runtime's current one (`Runtime::heap`, the
    // `HeapLayout` contract), and `rt` is used only through `rt_ptr` until
    // the call ends.
    let code: extern "C" fn(*mut Ctx<V>) -> u32 = unsafe { std::mem::transmute(mem.ptr) };
    let r = code(&mut ctx);
    // SAFETY: machine code has returned; `rt_ptr` is the live `&mut R`.
    let rt = unsafe { &mut *rt_ptr };
    rt.set_interrupt_counter(ctx.counter);
    if let Some(payload) = ctx.panic.take() {
        resume_unwind(payload);
    }
    let exit = match r {
        EXIT_RETURN => Exit::Return,
        EXIT_FRAME => Exit::Frame,
        EXIT_FAULT => Exit::Fault,
        EXIT_DONE => Exit::Done,
        _ => Exit::Interp,
    };
    Outcome {
        exit,
        pc: ctx.pc,
        acc: ctx.acc,
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
/// machine code.
fn call_out<V: JitValue, R: Runtime<V>>(
    ctx: *mut Ctx<V>,
    f: impl FnOnce(&mut R, V) -> Resume<V>,
) -> u32 {
    // SAFETY: machine code passes the `Ctx` `enter` gave it, which is live
    // and not otherwise borrowed while machine code waits in this call.
    let ctx = unsafe { &mut *ctx };
    // SAFETY: `ctx.rt` is the `&mut R` given to `enter`, which uses it only
    // through this pointer until machine code returns.
    let rt = unsafe { &mut *(ctx.rt as *mut R) };
    let acc = ctx.acc;
    let counter = ctx.counter;
    let registers = ctx.registers;
    let heap = ctx.heap;
    let r = catch_unwind(AssertUnwindSafe(|| {
        rt.set_interrupt_counter(counter);
        let resume = f(rt, acc);
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
    match r {
        Ok((resume, counter, attend, window)) => {
            ctx.counter = counter;
            ctx.attend = attend;
            match resume {
                Resume::Next { pc, acc } => {
                    ctx.acc = acc;
                    if let Some((w, view)) = window {
                        ctx.regs = w;
                        ctx.objects = view.objects;
                        ctx.proto_epoch = view.proto_epoch;
                    }
                    pc
                }
                Resume::Exit(e) => exit_code(e),
            }
        }
        Err(payload) => {
            ctx.panic = Some(payload);
            EXIT_PANIC
        }
    }
}

extern "C" fn slow_trampoline<V: JitValue, R: Runtime<V>>(ctx: *mut Ctx<V>, pc: u32) -> u32 {
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
