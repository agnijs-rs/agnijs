//! The engine's only unsafe code (ADR 0019 J10, ADR 0020): executable
//! memory, the entry into machine code, and the trampolines machine code
//! calls back through. Everything else in the engine forbids `unsafe`.
//!
//! The obligations, all discharged here:
//!
//! * Executable memory is never writable and executable at once (W^X): it
//!   is written while read-write, then made read-execute, and the
//!   instruction cache is synchronised before the first call.
//! * Machine code is only entered through [`enter`], with a [`Ctx`] that
//!   lives on `enter`'s stack frame for the whole run, whose register
//!   window has at least as many registers as the code addresses, and
//!   whose runtime pointer is the `&mut R` `enter` was given, used only
//!   through that pointer until the run ends.
//! * Machine code writes values only with the tags and payloads of
//!   [`crate::layout`], which [`layout_matches`] checks against the value
//!   type's own constructors before any code for that type is generated.
//! * No panic crosses machine code: each trampoline catches it, parks the
//!   payload in the `Ctx`, and returns `EXIT_PANIC`; `enter` resumes the
//!   unwind once machine code has returned.
#![allow(unsafe_code)]

use std::any::Any;
use std::ffi::c_void;
use std::mem::{align_of, offset_of, size_of};
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};

use crate::{layout, Exit, JitValue, Outcome, Resume, Runtime};

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
    /// The accumulator, whenever control is outside machine code.
    acc: V,
    /// Registers the code addresses; a shorter window is a runtime bug.
    registers: usize,
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
        acc: offset_of!(Ctx<V>, acc) as i32,
    }
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
        let ptr = sys::alloc_rw(len)?;
        // SAFETY: `ptr` is a fresh read-write mapping of at least `len`
        // bytes that nothing else refers to.
        unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), ptr, len) };
        if !sys::make_rx(ptr, len) {
            sys::free(ptr, len);
            return None;
        }
        sys::flush_icache(ptr, len);
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
    registers: usize,
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
    let window = rt.registers();
    if window.len() < registers {
        return Outcome {
            exit: Exit::Interp,
            pc,
            acc,
        };
    }
    let regs = window.as_mut_ptr();
    let counter = rt.interrupt_counter();
    let rt_ptr: *mut R = rt;
    let mut ctx = Ctx {
        regs,
        rt: rt_ptr as *mut c_void,
        slow: slow_trampoline::<V, R>,
        poll: poll_trampoline::<V, R>,
        arith: arith_trampoline::<V, R>,
        pc,
        counter,
        acc,
        registers,
        panic: None,
    };
    // SAFETY: `mem` holds code generated by `codegen::compile` for this
    // `V` with the offsets of this `Ctx<V>`, and `ctx` satisfies the
    // module's obligations: it outlives the call, its window is large
    // enough, and `rt` is used only through `rt_ptr` until the call ends.
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
    let r = catch_unwind(AssertUnwindSafe(|| {
        rt.set_interrupt_counter(counter);
        let resume = f(rt, acc);
        let counter = rt.interrupt_counter();
        let window = match resume {
            Resume::Next { .. } => {
                let w = rt.registers();
                assert!(w.len() >= registers, "register window shrank under compiled code");
                Some(w.as_mut_ptr())
            }
            Resume::Exit(_) => None,
        };
        (resume, counter, window)
    }));
    match r {
        Ok((resume, counter, window)) => {
            ctx.counter = counter;
            match resume {
                Resume::Next { pc, acc } => {
                    ctx.acc = acc;
                    if let Some(w) = window {
                        ctx.regs = w;
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
