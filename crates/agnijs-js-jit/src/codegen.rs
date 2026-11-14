//! Bytecode to Cranelift IR (ADR 0020). Safe code: it produces bytes,
//! which [`crate::exec`] maps and runs.
//!
//! One machine function per code block, `extern "C" fn(*mut Ctx) -> u32`.
//! It starts with a switch on the entry pc (the function's start, loop
//! headers, and every instruction after one that may call out) to a resume
//! block that loads the register window and the accumulator from the
//! context. Each instruction is a block; the accumulator lives in two
//! 64-bit variables (tag word, payload word) between instructions and is
//! flushed to the context before any call out.
//!
//! Registers are read and written in the register file itself, except the
//! few a block's loops use most ([`choose_cache`]), which live in
//! variables across instructions and call outs (TASK-101.3.1). Before a
//! call out, the cached registers the instruction reads are written back
//! to the file and the other dirty ones to the compiled frame's spill
//! area, listed in a [`Point`] the runtime reads through the frame's
//! handle (the GC map); after it, the registers the runtime may have
//! written are reloaded. A register the runtime speculated to hold a
//! number ([`Speculation`]) is cached unboxed: a store of anything else
//! into it, or entering compiled code while it holds anything else and is
//! read before written, is a guard failure, which materialises the frame
//! through the deopt helper and leaves for the interpreter before the
//! guarded instruction (a [`PointKind::Exit`]).
//!
//! Other fast paths are guarded by tag tests and never speculate: a failed
//! guard calls the runtime's `slow` entry, which runs the instruction's
//! generic semantics and returns the pc to continue at, or an exit code.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use agnijs_js_compiler::bytecode::{
    BinOp, CodeBlock, Constant, IcEntry, IcSite, IcState, Instr, IC_ENTRIES,
};
use cranelift_codegen::control::ControlPlane;
use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::types::{self, F64, I32, I64, I64X2};
use cranelift_codegen::ir::{
    AbiParam, Block, Function, InstBuilder, MemFlagsData, SigRef, Signature, StackSlot,
    StackSlotData, StackSlotKind, Type, UserFuncName, Value as Ir,
};
use cranelift_codegen::isa::OwnedTargetIsa;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Switch, Variable};

use crate::analysis::{
    access, always_inline, branch_offset, choose_cache, liveness, written, Access,
};
use crate::calls::{GlobalSite, CALLEE, CALLEE_BITS, CALLEE_HASH, CALLEE_WAYS, GLOBAL};
use crate::exec::{Offsets, EXIT_INTERP, EXIT_PANIC, EXIT_RETURN, NO_POINT};
use crate::handoff::{Location, Point, PointKind, Points};
use crate::layout::*;
use crate::{Arith, CallLayout, EnvLayout, HeapLayout, Speculation};

/// What [`compile`] produces.
pub(crate) struct Lowered {
    pub bytes: Vec<u8>,
    /// The pcs compiled code can be entered at, ascending.
    pub entries: Vec<u32>,
    /// The safe points, side exits and entry guards of the code.
    pub points: Points,
    /// How many registers the code keeps in machine state.
    pub cached: usize,
}

/// What lets the code make calls itself: the runtime's frame stack, and
/// the block's global sites, one per `LdaGlobal` in pc order, which the
/// code addresses where they are (they outlive it).
pub(crate) struct Calls<'a> {
    pub layout: &'a CallLayout,
    pub globals: &'a [GlobalSite],
    /// The address of the call thunk ([`compile_thunk`]) for this layout,
    /// 0 for none (every call is then the runtime's).
    pub thunk: usize,
}

/// The host code generator, configured once per process.
fn isa() -> Result<&'static OwnedTargetIsa, String> {
    static ISA: OnceLock<Result<OwnedTargetIsa, String>> = OnceLock::new();
    ISA.get_or_init(|| {
        let mut flags = settings::builder();
        let set = |f: &mut settings::Builder, k: &str, v: &str| {
            f.set(k, v).map_err(|e| format!("cranelift setting {k}={v}: {e}"))
        };
        set(&mut flags, "opt_level", "speed")?;
        // No unwind tables: no panic or exception unwinds through machine
        // code (the trampolines catch them, exec.rs).
        set(&mut flags, "unwind_info", "false")?;
        set(
            &mut flags,
            "enable_verifier",
            if cfg!(debug_assertions) { "true" } else { "false" },
        )?;
        let builder = cranelift_native::builder().map_err(|e| e.to_string())?;
        builder
            .finish(settings::Flags::new(flags))
            .map_err(|e| e.to_string())
    })
    .as_ref()
    .map_err(|e| e.clone())
}

/// Generate machine code for a finished IR function. Code that needs
/// relocations (a libcall the lowering did not avoid) is refused: compiled
/// code is position-independent bytes.
fn emit(func: Function) -> Result<Vec<u8>, String> {
    let isa = isa()?;
    let mut ctx = Context::for_function(func);
    let compiled = ctx
        .compile(&**isa, &mut ControlPlane::default())
        .map_err(|e| format!("{:?}", e.inner))?;
    if !compiled.buffer.relocs().is_empty() {
        return Err("generated code needs relocations".into());
    }
    Ok(compiled.code_buffer().to_vec())
}

/// The start-up probe's function: `extern "C" fn() -> u32 { 42 }`.
pub(crate) fn probe_code() -> Result<Vec<u8>, String> {
    let isa = isa()?;
    let mut sig = Signature::new(isa.default_call_conv());
    sig.returns.push(AbiParam::new(I32));
    let mut func = Function::with_name_signature(UserFuncName::default(), sig);
    let mut fctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut func, &mut fctx);
        let entry = b.create_block();
        b.switch_to_block(entry);
        let v = b.ins().iconst(I32, 42);
        b.ins().return_(&[v]);
        b.seal_all_blocks();
        b.finalize(isa.frontend_config());
    }
    emit(func)
}

/// A jump target, or an error for one outside the block.
fn target(len: usize, pc: usize, off: i32) -> Result<usize, String> {
    let t = pc as i64 + off as i64;
    if t < 0 || t as usize >= len {
        return Err(format!("jump at {pc} leaves the block"));
    }
    Ok(t as usize)
}

/// The entry pcs of a block: its start, the target of every back-edge
/// (on-stack replacement from the interpreter's loop), and every pc the
/// runtime may continue at after a call out.
fn entries(code: &CodeBlock) -> Result<BTreeSet<u32>, String> {
    let len = code.instrs.len();
    let mut set = BTreeSet::new();
    set.insert(0u32);
    for (pc, instr) in code.instrs.iter().enumerate() {
        let taken = match branch_offset(instr) {
            Some(off) => Some((target(len, pc, off)?, off)),
            None => None,
        };
        if let Some((t, off)) = taken {
            if off <= 0 {
                set.insert(t as u32);
            }
        }
        if !always_inline(code, instr) {
            if pc + 1 < len {
                set.insert(pc as u32 + 1);
            }
            if let Some((t, _)) = taken {
                set.insert(t as u32);
            }
        }
    }
    Ok(set)
}

/// How many entries of each inline-cache site compiled code tests inline:
/// all of them (the first empty one ends the search at run time, so a
/// monomorphic site costs one test), none for a site that was already
/// megamorphic when the block was compiled, which stays so.
fn ic_unroll(code: &CodeBlock) -> Vec<u8> {
    let n = code.ic_count as usize;
    let fb = code.feedback.get();
    (0..n)
        .map(|i| match fb.and_then(|f| f.ic.get(i)) {
            Some(site) if site.state() == IcState::Megamorphic => 0,
            _ => IC_ENTRIES as u8,
        })
        .collect()
}

/// Compile `code` against a context with field offsets `off`, with the
/// inline-cache hit paths when `heap` describes the runtime's objects and
/// calls made in machine code when `calls` describes its frame stack.
pub(crate) fn compile(
    code: &CodeBlock,
    off: &Offsets,
    heap: Option<&HeapLayout>,
    calls: Option<&Calls<'_>>,
    spec: &Speculation,
) -> Result<Lowered, String> {
    let isa = isa()?;
    let ptr = isa.pointer_type();
    let cc = isa.default_call_conv();
    let entries = entries(code)?;
    let chosen = choose_cache(code, spec);
    let mut index = vec![None; code.register_count as usize];
    for (i, &(r, _)) in chosen.iter().enumerate() {
        match index.get_mut(r as usize) {
            Some(slot) => *slot = Some(i as u8),
            None => return Err(format!("register {r} is outside the block's window")),
        }
    }
    let live_in = liveness(code, &index, chosen.len());

    let mut sig = Signature::new(cc);
    sig.params.push(AbiParam::new(ptr));
    sig.params.push(AbiParam::new(ptr));
    sig.returns.push(AbiParam::new(I32));
    let fn_sig = sig.clone();
    let mut thunk_sig = sig.clone();
    thunk_sig.params.push(AbiParam::new(I64));
    thunk_sig.params.push(AbiParam::new(I32));
    // Frames, environments and global sites are read as 64-bit words: on
    // 64-bit hosts (the only ones there are).
    let calls = calls.filter(|_| ptr == I64 && heap.is_some());
    let mut slow_sig = Signature::new(cc);
    slow_sig.params.push(AbiParam::new(ptr));
    slow_sig.params.push(AbiParam::new(I32));
    slow_sig.returns.push(AbiParam::new(I32));
    let mut poll_sig = slow_sig.clone();
    poll_sig.params.push(AbiParam::new(I32));
    let mut arith_sig = Signature::new(cc);
    arith_sig.params.push(AbiParam::new(I32));
    arith_sig.params.push(AbiParam::new(F64));
    arith_sig.params.push(AbiParam::new(F64));
    arith_sig.returns.push(AbiParam::new(F64));

    let mut func = Function::with_name_signature(UserFuncName::default(), sig);
    let mut fctx = FunctionBuilderContext::new();
    let points_out;
    {
        let mut b = FunctionBuilder::new(&mut func, &mut fctx);
        let slow_sig = b.import_signature(slow_sig);
        let poll_sig = b.import_signature(poll_sig);
        let arith_sig = b.import_signature(arith_sig);
        let fn_sig = b.import_signature(fn_sig);
        let thunk_sig = b.import_signature(thunk_sig);
        let regs = b.declare_var(ptr);
        let tag = b.declare_var(I64);
        let pay = b.declare_var(I64);
        let cached: Vec<Cached> = chosen
            .iter()
            .map(|&(reg, number)| Cached {
                reg,
                repr: if number {
                    Repr::Number {
                        bits: b.declare_var(I64),
                        tag: b.declare_var(I64),
                    }
                } else {
                    Repr::Boxed {
                        tag: b.declare_var(I64),
                        pay: b.declare_var(I64),
                    }
                },
                dirty: code.instrs.iter().any(|i| written(i) == Some(reg)),
            })
            .collect();
        let spill = (!cached.is_empty()).then(|| {
            b.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                (cached.len() * SIZE) as u32,
                4,
            ))
        });

        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        let len = code.instrs.len();
        // blocks[len]: falling off the end, which valid bytecode never does.
        let blocks: Vec<Block> = (0..=len).map(|_| b.create_block()).collect();
        let mut resume = vec![None; len];
        for &pc in &entries {
            resume[pc as usize] = Some(b.create_block());
        }

        b.switch_to_block(entry);
        let ctx = b.block_params(entry)[0];
        let sh = b.block_params(entry)[1];
        let number_tag = b.ins().iconst(I64, TAG_NUMBER as i64);
        let mut lower = Lower {
            b,
            code,
            off,
            ptr,
            ctx,
            sh,
            number_tag,
            calls: calls.map(|c| *c.layout),
            globals: calls.map_or(0, |c| c.globals.as_ptr() as usize),
            global: 0,
            thunk: calls.map_or(0, |c| c.thunk),
            thunk_sig,
            fn_sig,
            regs,
            tag,
            pay,
            cached,
            index,
            live_in,
            spill,
            in_file: Vec::new(),
            points: Vec::new(),
            blocks,
            resume,
            slow_sig,
            poll_sig,
            arith_sig,
            heap: heap.copied(),
            unroll: ic_unroll(code),
        };
        if let Some(ss) = lower.spill {
            // The spill area's address, fixed for the frame, where the
            // runtime finds the registers a point lists.
            let a = lower.b.ins().stack_addr(ptr, ss, 0);
            lower.b.ins().store(flags(), a, ctx, off.spill);
        }
        lower.dispatch(&entries);
        for (pc, &instr) in code.instrs.iter().enumerate() {
            let blk = lower.blocks[pc];
            lower.b.switch_to_block(blk);
            lower.instr(pc, instr)?;
        }
        let end = lower.blocks[len];
        lower.b.switch_to_block(end);
        let v = lower.b.ins().iconst(I32, EXIT_PANIC as i64);
        lower.b.ins().return_(&[v]);
        lower.b.seal_all_blocks();
        lower.b.finalize(isa.frontend_config());
        points_out = lower.points;
    }
    let points = Points { points: points_out };
    let bytes = emit(func)?;
    Ok(Lowered {
        bytes,
        entries: entries.into_iter().collect(),
        points,
        cached: chosen.len(),
    })
}

/// Compile the call thunk for a runtime's layouts: machine code every
/// `Call` of code compiled with them calls, as
/// `extern "C" fn(*mut Ctx, *mut Shared, ops: u64, pc: u32) -> u32`, to
/// make the call without leaving compiled code when it can, and through
/// the runtime when it cannot ([`Lower::thunk`]). It answers as a runtime
/// entry does: the pc to continue at, the accumulator and the register
/// window being in the context, or an exit code. The pc is always one of
/// the calling frame's: the callee's own answer never reaches the call
/// site as it is. A callee that returned is popped and the answer is the
/// caller's `pc + 1`; one that left any other way may have answered a pc
/// of its own block (code that keeps nothing in machine state does, for a
/// pc it has no block for), and the thunk's "left" path makes whatever it
/// answered an exit code first (`exec::left_code`, TASK-103.36). (`code`
/// is any block: the thunk is not about one.)
pub(crate) fn compile_thunk(
    code: &CodeBlock,
    off: &Offsets,
    heap: &HeapLayout,
    calls: &CallLayout,
) -> Result<Vec<u8>, String> {
    let isa = isa()?;
    let ptr = isa.pointer_type();
    if ptr != I64 {
        return Err("the call thunk needs a 64-bit host".into());
    }
    let cc = isa.default_call_conv();
    let mut fn_sig = Signature::new(cc);
    fn_sig.params.push(AbiParam::new(ptr));
    fn_sig.params.push(AbiParam::new(ptr));
    fn_sig.returns.push(AbiParam::new(I32));
    let mut sig = fn_sig.clone();
    sig.params.push(AbiParam::new(I64));
    sig.params.push(AbiParam::new(I32));
    let mut slow_sig = Signature::new(cc);
    slow_sig.params.push(AbiParam::new(ptr));
    slow_sig.params.push(AbiParam::new(I32));
    slow_sig.returns.push(AbiParam::new(I32));
    let mut poll_sig = slow_sig.clone();
    poll_sig.params.push(AbiParam::new(I32));
    let mut arith_sig = Signature::new(cc);
    arith_sig.params.push(AbiParam::new(I32));
    arith_sig.params.push(AbiParam::new(F64));
    arith_sig.params.push(AbiParam::new(F64));
    arith_sig.returns.push(AbiParam::new(F64));

    let mut func = Function::with_name_signature(UserFuncName::default(), sig.clone());
    let mut fctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut func, &mut fctx);
        let slow_sig = b.import_signature(slow_sig);
        let poll_sig = b.import_signature(poll_sig);
        let arith_sig = b.import_signature(arith_sig);
        let fn_sig = b.import_signature(fn_sig);
        let thunk_sig = b.import_signature(sig);
        let regs = b.declare_var(ptr);
        let tag = b.declare_var(I64);
        let pay = b.declare_var(I64);
        // The context of the function being called.
        let slot = b.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            off.ctx_size,
            4,
        ));
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let (ctx, sh) = (b.block_params(entry)[0], b.block_params(entry)[1]);
        let (ops, pc) = (b.block_params(entry)[2], b.block_params(entry)[3]);
        let number_tag = b.ins().iconst(I64, TAG_NUMBER as i64);
        let mut lower = Lower {
            b,
            code,
            off,
            ptr,
            ctx,
            sh,
            number_tag,
            calls: Some(*calls),
            globals: 0,
            global: 0,
            thunk: 0,
            thunk_sig,
            fn_sig,
            regs,
            tag,
            pay,
            cached: Vec::new(),
            index: Vec::new(),
            live_in: Vec::new(),
            spill: None,
            in_file: Vec::new(),
            points: Vec::new(),
            blocks: Vec::new(),
            resume: Vec::new(),
            slow_sig,
            poll_sig,
            arith_sig,
            heap: Some(*heap),
            unroll: Vec::new(),
        };
        // The caller's window and accumulator, as its call site left them
        // in its context.
        lower.reload();
        lower.thunk(heap, calls, slot, ops, pc);
        lower.b.seal_all_blocks();
        lower.b.finalize(isa.frontend_config());
    }
    emit(func)
}

/// An operand of a binary operator.
#[derive(Clone, Copy)]
enum Src {
    Acc,
    Reg(u16),
    Imm(i32),
}

/// A value in two machine words. `tag` is `None` for a value its readers
/// take as a number without a test: an immediate, or a register speculated
/// to hold one. `word` is then the tag word that goes with the payload
/// wherever the value is stored and not computed with (`None`: plainly
/// [`TAG_NUMBER`]); see [`Repr::Number`].
#[derive(Clone, Copy)]
struct Val {
    tag: Option<Ir>,
    pay: Ir,
    word: Option<Ir>,
}

/// How a cached register is held.
#[derive(Clone, Copy)]
enum Repr {
    /// Tag word and payload word, as in the register file.
    Boxed { tag: Variable, pay: Variable },
    /// The register was speculated to hold a number: `bits` is the payload
    /// word, which the instructions that compute with the register read as
    /// an `f64` without a tag test. `tag` is the tag word that payload came
    /// with: [`Lower::number_tag`] once the register has been guarded or
    /// written, and until then the word loaded from the register file,
    /// where the register was dead and so not guarded.
    ///
    /// The two together are always the register's value (TASK-103.34).
    /// Whatever moves the value without computing with it (a spill at a
    /// point, a write-back, a copy into another register or the
    /// accumulator) moves both, so a register nothing has guarded yet
    /// reaches the interpreter frame, a handler or a debugger as what it
    /// held, never as that value's payload under a number's tag. The
    /// liveness analysis only decides where a tag test may be skipped.
    Number { bits: Variable, tag: Variable },
}

/// A register kept in machine state.
#[derive(Clone, Copy)]
struct Cached {
    reg: u16,
    repr: Repr,
    /// Written by some inline-expanded instruction, so possibly newer in
    /// machine state than in the register file.
    dirty: bool,
}

struct Lower<'a, 'c> {
    b: FunctionBuilder<'a>,
    code: &'c CodeBlock,
    off: &'c Offsets,
    ptr: Type,
    /// This frame's context and the run's shared state (`exec::Ctx`,
    /// `exec::Shared`): the function's two arguments.
    ctx: Ir,
    sh: Ir,
    /// The tag word of a number, as one value defined at the function's
    /// start: the tag of every speculated register that has been guarded or
    /// written ([`Repr::Number`]). One value, so that where every path
    /// agrees on it the tag is a constant and costs nothing.
    number_tag: Ir,
    /// The runtime's frame stack, for calls made in machine code.
    calls: Option<CallLayout>,
    /// The address of the block's first global site, and the index of the
    /// next `LdaGlobal`'s.
    globals: usize,
    global: u32,
    /// The call thunk's address (0 for none) and signature.
    thunk: usize,
    thunk_sig: SigRef,
    /// The signature of compiled code.
    fn_sig: SigRef,
    regs: Variable,
    tag: Variable,
    pay: Variable,
    /// The registers held in machine state, by cache index.
    cached: Vec<Cached>,
    /// The cache index of each register of the window.
    index: Vec<Option<u8>>,
    /// Per pc, the cached registers live on entry ([`liveness`]).
    live_in: Vec<u16>,
    /// The spill area: [`SIZE`] bytes per cache index.
    spill: Option<StackSlot>,
    /// Per cache index, where the last point emitted left the register's
    /// value: in the register file (it was written back for the
    /// instruction to read, or compiled code never writes it), else in its
    /// spill slot. What [`Lower::reload_cached`] reads after the call.
    in_file: Vec<bool>,
    /// The points emitted so far; a point's id is its index.
    points: Vec<Point>,
    blocks: Vec<Block>,
    resume: Vec<Option<Block>>,
    slow_sig: SigRef,
    poll_sig: SigRef,
    arith_sig: SigRef,
    /// The runtime's object layout, for the inline-cache hit paths.
    heap: Option<HeapLayout>,
    /// Entries tested inline per inline-cache site ([`ic_unroll`]).
    unroll: Vec<u8>,
}

fn flags() -> MemFlagsData {
    MemFlagsData::trusted()
}

impl Lower<'_, '_> {
    // ---- entry ----------------------------------------------------------

    fn dispatch(&mut self, entries: &BTreeSet<u32>) {
        let pc = self.b.ins().load(I32, flags(), self.ctx, self.off.pc);
        // A call starts at pc 0: tested first, so that a function entered
        // by calls pays one predictable branch for the dispatch.
        let start = self.resume[0].unwrap();
        let later = self.b.create_block();
        self.b.ins().brif(pc, later, &[], start, &[]);
        self.b.switch_to_block(later);
        let default = self.b.create_block();
        let mut sw = Switch::new();
        for &e in entries {
            sw.set_entry(e as u128, self.resume[e as usize].unwrap());
        }
        sw.emit(&mut self.b, pc, default);
        self.b.switch_to_block(default);
        let v = self.b.ins().iconst(I32, EXIT_INTERP as i64);
        self.b.ins().return_(&[v]);
        for &e in entries {
            let blk = self.resume[e as usize].unwrap();
            self.b.switch_to_block(blk);
            self.reload();
            // Entered from the interpreter: the register file is complete.
            self.load_cached(e as usize, PointKind::Entry { guard: 0 });
            let next = self.blocks[e as usize];
            self.b.ins().jump(next, &[]);
        }
    }

    /// Load the register window and the accumulator from the context.
    fn reload(&mut self) {
        let regs = self.b.ins().load(self.ptr, flags(), self.ctx, self.off.regs);
        self.b.def_var(self.regs, regs);
        let t = self.b.ins().load(I64, flags(), self.ctx, self.off.acc);
        let p = self.b.ins().load(I64, flags(), self.ctx, self.off.acc + 8);
        self.b.def_var(self.tag, t);
        self.b.def_var(self.pay, p);
    }

    fn flush_acc(&mut self) {
        let t = self.b.use_var(self.tag);
        let p = self.b.use_var(self.pay);
        self.store_value(self.ctx, self.off.acc, t, p);
    }

    // ---- cached registers ---------------------------------------------------

    fn cached(&self, r: u16) -> Option<Cached> {
        let i = self.index.get(r as usize).copied().flatten()?;
        Some(self.cached[i as usize])
    }

    fn live_at(&self, pc: usize, i: usize) -> bool {
        self.live_in.get(pc).is_some_and(|m| m & (1 << i) != 0)
    }

    /// Load every cached register from the register file, to continue at
    /// `pc`. A speculated register is guarded when it is live there: a
    /// non-number leaves through a point of `kind` (its guard being the
    /// register) at `pc`, nothing being dirty. One dead there is written
    /// before anything reads it, on every path and after a throw too, so
    /// no test is spent on it: it is taken as it is, tag word and payload,
    /// and stays whatever it was until it is written ([`Repr::Number`]).
    fn load_cached(&mut self, pc: usize, kind: PointKind) {
        for i in 0..self.cached.len() {
            let c = self.cached[i];
            let (t, p) = self.load_file(c.reg);
            match c.repr {
                Repr::Boxed { tag, pay } => {
                    self.b.def_var(tag, t);
                    self.b.def_var(pay, p);
                }
                Repr::Number { bits, tag } => {
                    let mut word = t;
                    if self.live_at(pc, i) {
                        word = self.number_tag;
                        let kind = if matches!(kind, PointKind::Entry { .. }) {
                            PointKind::Entry { guard: c.reg }
                        } else {
                            PointKind::Exit { guard: Some(c.reg) }
                        };
                        let ok = self.is_tag(t, TAG_NUMBER);
                        let (yes, no) = self.guard(ok);
                        self.b.set_cold_block(no);
                        self.b.switch_to_block(no);
                        self.leave_at(pc, kind, false);
                        self.b.switch_to_block(yes);
                    }
                    self.b.def_var(bits, p);
                    self.b.def_var(tag, word);
                }
            }
        }
    }

    /// The spill slot's address for cache index `i`.
    fn spill_addr(&mut self, i: usize) -> (Ir, i32) {
        let ss = self.spill.expect("a spill area for cached registers");
        let a = self.b.ins().stack_addr(self.ptr, ss, 0);
        (a, (i * SIZE) as i32)
    }

    /// A point at `pc`, with `dirty` when cached registers may be newer in
    /// machine state than in the register file: write every dirty one the
    /// instruction `reads` back to the file and spill the rest, listing
    /// them (dead ones too: the frame the interpreter gets is complete, as
    /// a debugger expects it); store the point's id in the context.
    /// Returns the id.
    ///
    /// Every register is spilled as a value, tag word and payload. A
    /// speculated one's tag is a number's only once it has been guarded or
    /// written ([`Repr::Number`]); one taken unguarded is listed as what
    /// the register file held, which is what it still holds.
    fn point(&mut self, pc: usize, kind: PointKind, dirty: Option<&Access>) -> u32 {
        let mut regs = Vec::new();
        self.in_file.clear();
        self.in_file.resize(self.cached.len(), true);
        for i in 0..self.cached.len() {
            let c = self.cached[i];
            let Some(reads) = dirty else { break };
            if !c.dirty {
                continue;
            }
            let read = match reads {
                Access::Any => true,
                Access::Known(list) => list.contains(&c.reg),
            };
            if read {
                self.write_back(c);
                continue;
            }
            self.in_file[i] = false;
            let (base, off) = self.spill_addr(i);
            match c.repr {
                Repr::Boxed { tag, pay } => {
                    let t = self.b.use_var(tag);
                    let p = self.b.use_var(pay);
                    self.store_value(base, off, t, p);
                    regs.push((c.reg, Location::Spill(off)));
                }
                Repr::Number { bits, tag } => {
                    // Two plain stores: the tag is nearly always the
                    // constant, and Rust reads a spill slot only when the
                    // frame is materialised or traced, long after.
                    let t = self.b.use_var(tag);
                    let p = self.b.use_var(bits);
                    self.b
                        .ins()
                        .store(flags(), t, base, off + TAG_OFFSET as i32);
                    self.b
                        .ins()
                        .store(flags(), p, base, off + PAYLOAD_OFFSET as i32);
                    regs.push((c.reg, Location::Spill(off)));
                }
            }
        }
        let id = self.points.len() as u32;
        self.points.push(Point {
            pc: pc as u32,
            kind,
            regs,
        });
        let v = self.b.ins().iconst(I32, id as i64);
        self.b.ins().store(flags(), v, self.ctx, self.off.point);
        id
    }

    /// Write a cached register's value to the register file.
    fn write_back(&mut self, c: Cached) {
        let (t, p) = match c.repr {
            Repr::Boxed { tag, pay } => (self.b.use_var(tag), self.b.use_var(pay)),
            Repr::Number { bits, tag } => (self.b.use_var(tag), self.b.use_var(bits)),
        };
        self.store_file(c.reg, t, p);
    }

    /// Leave for the interpreter at `pc` through a point of `kind` (a
    /// side exit or an entry guard), in the current block: the dirty
    /// registers live at `pc` are spilled and listed (none, when
    /// `dirty` is false), the frame is materialised by the runtime's
    /// deopt helper and machine code returns what it says.
    fn leave_at(&mut self, pc: usize, kind: PointKind, dirty: bool) {
        self.flush_acc();
        let none = Access::Known(Vec::new());
        let id = self.point(pc, kind, dirty.then_some(&none));
        let f = self
            .b
            .ins()
            .load(self.ptr, flags(), self.sh, self.off.s_deopt);
        let a = self.b.ins().iconst(I32, id as i64);
        let call = self.b.ins().call_indirect(self.slow_sig, f, &[self.ctx, a]);
        let r = self.b.inst_results(call)[0];
        self.b.ins().return_(&[r]);
    }

    /// Reload after the runtime ran the instruction at `pc` and continues
    /// at `to`: a generic instruction may have written any register, so
    /// every cached one is reloaded (speculated ones guarded, a failure
    /// leaving at `to` with nothing dirty); a known one wrote none.
    ///
    /// After a known one the cached registers are read back all the same,
    /// each from where the call's point put it ([`Lower::in_file`]): the
    /// register file, or its spill slot, which nothing writes while the
    /// call runs. The values are the ones machine code held before the
    /// call. Reading them back ends every cached register's life in a
    /// machine register at the call, which is on a cold path: without
    /// that each one is live across every call out of the block, so the
    /// register allocator must keep it in a register calls preserve or on
    /// the stack, and with more such values than such registers it put
    /// the ones a hot loop reads on the stack (TASK-103.56). Only the
    /// context pointers stay live across a call now.
    ///
    /// A speculated register's tag word is not read back: it is a constant
    /// wherever the register has been guarded or written, and reading it
    /// would make it a value to merge after every call out
    /// ([`Repr::Number`]).
    fn reload_cached(&mut self, pc: usize, to: usize) {
        if let Access::Any = access(&self.code.instrs[pc]) {
            self.load_cached(to, PointKind::Exit { guard: None });
            return;
        }
        for i in 0..self.cached.len() {
            let c = self.cached[i];
            let (base, off) = if self.in_file.get(i).copied().unwrap_or(true) {
                (self.b.use_var(self.regs), c.reg as i32 * SIZE as i32)
            } else {
                self.spill_addr(i)
            };
            let p = self
                .b
                .ins()
                .load(I64, flags(), base, off + PAYLOAD_OFFSET as i32);
            match c.repr {
                Repr::Boxed { tag, pay } => {
                    let t = self.b.ins().load(I64, flags(), base, off + TAG_OFFSET as i32);
                    self.b.def_var(tag, t);
                    self.b.def_var(pay, p);
                }
                Repr::Number { bits, .. } => self.b.def_var(bits, p),
            }
        }
    }

    // ---- values -----------------------------------------------------------

    fn load_file(&mut self, r: u16) -> (Ir, Ir) {
        let regs = self.b.use_var(self.regs);
        let o = r as i32 * SIZE as i32;
        let t = self.b.ins().load(I64, flags(), regs, o + TAG_OFFSET as i32);
        let p = self.b.ins().load(I64, flags(), regs, o + PAYLOAD_OFFSET as i32);
        (t, p)
    }

    fn store_file(&mut self, r: u16, t: Ir, p: Ir) {
        let regs = self.b.use_var(self.regs);
        let o = r as i32 * SIZE as i32;
        self.store_value(regs, o, t, p);
    }

    /// Register `r` as it is, tag word and payload.
    fn load_reg(&mut self, r: u16) -> (Ir, Ir) {
        let v = self.read_reg(r);
        (self.tag_word(v), v.pay)
    }

    /// The tag word to store with `v`: its own, or a number's.
    fn tag_word(&mut self, v: Val) -> Ir {
        v.tag.or(v.word).unwrap_or(self.number_tag)
    }

    /// Register `r` as a [`Val`]: without a tag when it is a speculated
    /// number, which its readers then need not test.
    fn read_reg(&mut self, r: u16) -> Val {
        match self.cached(r).map(|c| c.repr) {
            Some(Repr::Boxed { tag, pay }) => Val {
                tag: Some(self.b.use_var(tag)),
                pay: self.b.use_var(pay),
                word: None,
            },
            Some(Repr::Number { bits, tag }) => Val {
                tag: None,
                pay: self.b.use_var(bits),
                word: Some(self.b.use_var(tag)),
            },
            None => {
                let (t, p) = self.load_file(r);
                Val {
                    tag: Some(t),
                    pay: p,
                    word: None,
                }
            }
        }
    }

    /// `r = v` by the instruction at `pc`: into a speculated register only
    /// a number goes; anything else is the guard failure that leaves at
    /// `pc`, before the instruction. (A value taken as a number without a
    /// test, another speculated register's, is copied with the tag word it
    /// has.)
    fn store_reg(&mut self, pc: usize, r: u16, v: Val) {
        match self.cached(r).map(|c| c.repr) {
            Some(Repr::Boxed { tag, pay }) => {
                let t = self.tag_word(v);
                self.b.def_var(tag, t);
                self.b.def_var(pay, v.pay);
            }
            Some(Repr::Number { bits, tag }) => {
                let mut word = v.word.unwrap_or(self.number_tag);
                if let Some(t) = v.tag {
                    let ok = self.is_tag(t, TAG_NUMBER);
                    let (yes, no) = self.guard(ok);
                    self.b.set_cold_block(no);
                    self.b.switch_to_block(no);
                    self.leave_at(pc, PointKind::Exit { guard: Some(r) }, true);
                    self.b.switch_to_block(yes);
                    word = self.number_tag;
                }
                self.b.def_var(bits, v.pay);
                self.b.def_var(tag, word);
            }
            None => {
                let t = self.tag_word(v);
                self.store_file(r, t, v.pay);
            }
        }
    }

    /// Store a value with one 16-byte store: Rust reads values whole, and
    /// a load spanning two narrower stores still in flight misses store
    /// forwarding (a stall measured on every call out, as the runtime
    /// reads back the accumulator or a register just written).
    fn store_value(&mut self, base: Ir, off: i32, t: Ir, p: Ir) {
        const { assert!(TAG_OFFSET == 0 && PAYLOAD_OFFSET == 8) };
        let v = self.b.ins().scalar_to_vector(I64X2, t);
        let v = self.b.ins().insertlane(v, p, 1);
        self.b.ins().store(MemFlagsData::new().with_notrap(), v, base, off);
    }

    fn acc(&mut self) -> (Ir, Ir) {
        (self.b.use_var(self.tag), self.b.use_var(self.pay))
    }

    fn set_acc(&mut self, t: Ir, p: Ir) {
        self.b.def_var(self.tag, t);
        self.b.def_var(self.pay, p);
    }

    fn set_acc_const(&mut self, tag: u8, payload: u64) {
        let t = self.b.ins().iconst(I64, tag as i64);
        let p = self.b.ins().iconst(I64, payload as i64);
        self.set_acc(t, p);
    }

    fn set_acc_number(&mut self, f: Ir) {
        let t = self.b.ins().iconst(I64, TAG_NUMBER as i64);
        let p = self.b.ins().bitcast(I64, MemFlagsData::new(), f);
        self.set_acc(t, p);
    }

    fn set_acc_bool(&mut self, c: Ir) {
        let t = self.b.ins().iconst(I64, TAG_BOOL as i64);
        let p = self.b.ins().uextend(I64, c);
        self.set_acc(t, p);
    }

    fn read(&mut self, s: Src) -> Val {
        match s {
            Src::Acc => {
                let (t, p) = self.acc();
                Val {
                    tag: Some(t),
                    pay: p,
                    word: None,
                }
            }
            Src::Reg(r) => self.read_reg(r),
            Src::Imm(i) => Val {
                tag: None,
                pay: self.b.ins().iconst(I64, (i as f64).to_bits() as i64),
                word: None,
            },
        }
    }

    fn is_tag(&mut self, t: Ir, tag: u8) -> Ir {
        let m = self.b.ins().band_imm_u(t, 0xff);
        self.b.ins().icmp_imm_u(IntCC::Equal, m, tag as i64)
    }

    /// Whether every operand with a tag is a number (true for none).
    fn numbers(&mut self, vals: &[Val]) -> Option<Ir> {
        let mut acc: Option<Ir> = None;
        for v in vals {
            if let Some(t) = v.tag {
                let c = self.is_tag(t, TAG_NUMBER);
                acc = Some(match acc {
                    Some(a) => self.b.ins().band(a, c),
                    None => c,
                });
            }
        }
        acc
    }

    fn f64(&mut self, v: Val) -> Ir {
        self.b.ins().bitcast(F64, MemFlagsData::new(), v.pay)
    }

    // ---- control ------------------------------------------------------------

    fn next(&mut self, pc: usize) {
        let blk = self.blocks[pc + 1];
        self.b.ins().jump(blk, &[]);
    }

    /// Branch on `cond` to a fresh block (returned, not yet filled) or on.
    fn guard(&mut self, cond: Ir) -> (Block, Block) {
        let yes = self.b.create_block();
        let no = self.b.create_block();
        self.b.ins().brif(cond, yes, &[], no, &[]);
        (yes, no)
    }

    /// Take the edge from the jump at `pc` to `to` in the current block:
    /// a back-edge advances the interrupt counter and polls when it is due
    /// (ADR 0019 J7), like the interpreter's jump.
    fn edge(&mut self, pc: usize, to: usize) {
        if to > pc {
            let blk = self.blocks[to];
            self.b.ins().jump(blk, &[]);
            return;
        }
        let c = self.b.ins().load(I32, flags(), self.sh, self.off.s_counter);
        let c = self.b.ins().iadd_imm_u(c, 1);
        self.b.ins().store(flags(), c, self.sh, self.off.s_counter);
        let low = self.b.ins().band_imm_u(c, 0xfff);
        let due = self.b.ins().icmp_imm_u(IntCC::Equal, low, 0);
        // Or a safe point is wanted at every back-edge (GC stress).
        let attend = self.b.ins().load(I32, flags(), self.sh, self.off.s_attend);
        let attend = self.b.ins().icmp_imm_u(IntCC::NotEqual, attend, 0);
        let due = self.b.ins().bor(due, attend);
        let poll = self.b.create_block();
        self.b.set_cold_block(poll);
        let blk = self.blocks[to];
        self.b.ins().brif(due, poll, &[], blk, &[]);
        self.b.switch_to_block(poll);
        self.flush_acc();
        // A poll reads and writes no register, but may collect.
        let none = Access::Known(Vec::new());
        self.point(
            pc,
            PointKind::SafePoint {
                next: vec![to as u32],
            },
            Some(&none),
        );
        let f = self
            .b
            .ins()
            .load(self.ptr, flags(), self.sh, self.off.s_poll);
        let a = self.b.ins().iconst(I32, pc as i64);
        let t = self.b.ins().iconst(I32, to as i64);
        let call = self.b.ins().call_indirect(self.poll_sig, f, &[self.ctx, a, t]);
        let r = self.b.inst_results(call)[0];
        self.continue_at(r, pc, &[to]);
    }

    /// Branch on `cond`: the jump at `pc` to `to` when true, else on.
    fn branch(&mut self, pc: usize, cond: Ir, to: usize) {
        let taken = self.b.create_block();
        let on = self.blocks[pc + 1];
        self.b.ins().brif(cond, taken, &[], on, &[]);
        self.b.switch_to_block(taken);
        self.edge(pc, to);
    }

    /// Run the instruction at `pc` by the runtime's generic semantics and
    /// continue where it says: `pc + 1`, the branch target, or out.
    fn slow(&mut self, pc: usize, to: Option<usize>) {
        self.call_out(self.off.s_slow, pc, to);
    }

    /// [`Lower::slow`] through the runtime entry at `entry` in the run's
    /// shared state.
    fn call_out(&mut self, entry: i32, pc: usize, to: Option<usize>) {
        self.flush_acc();
        let mut succ = vec![pc + 1];
        succ.extend(to);
        let reads = access(&self.code.instrs[pc]);
        let next = succ.iter().map(|&s| s as u32).collect();
        self.point(pc, PointKind::SafePoint { next }, Some(&reads));
        let f = self.b.ins().load(self.ptr, flags(), self.sh, entry);
        let a = self.b.ins().iconst(I32, pc as i64);
        let call = self.b.ins().call_indirect(self.slow_sig, f, &[self.ctx, a]);
        let r = self.b.inst_results(call)[0];
        self.continue_at(r, pc, &succ);
    }

    /// After a call out from the instruction at `pc` returned `r`: reload
    /// the context and whatever the runtime may have written, and continue
    /// at whichever successor `r` names; else return `r`. That is an exit
    /// code (the runtime materialised the frame for any pc it had no
    /// successor for) or, from code that keeps nothing in machine state, a
    /// pc of this block the runtime continued the frame at. `r` must be a
    /// word about this frame for the comparison to mean anything: every
    /// entry that answers here is called with this frame's context, and
    /// the call thunk translates what a callee answered ([`compile_thunk`]).
    fn continue_at(&mut self, r: Ir, pc: usize, succ: &[usize]) {
        for &s in succ {
            if s >= self.blocks.len() - 1 {
                continue;
            }
            let post = self.b.create_block();
            let other = self.b.create_block();
            let hit = self.b.ins().icmp_imm_u(IntCC::Equal, r, s as i64);
            self.b.ins().brif(hit, post, &[], other, &[]);
            self.b.switch_to_block(post);
            self.reload();
            self.reload_cached(pc, s);
            let blk = self.blocks[s];
            self.b.ins().jump(blk, &[]);
            self.b.switch_to_block(other);
        }
        self.b.ins().return_(&[r]);
    }

    /// The fast path in `fast` (filled by the caller after this returns,
    /// in the current block) when `cond`, else the generic path.
    fn fast_or_slow(&mut self, pc: usize, cond: Option<Ir>, to: Option<usize>) {
        let Some(cond) = cond else { return };
        let (fast, slow) = self.guard(cond);
        self.b.set_cold_block(slow);
        self.b.switch_to_block(slow);
        self.slow(pc, to);
        self.b.switch_to_block(fast);
    }

    // ---- operators ----------------------------------------------------------

    /// `acc = l OP r` with a number fast path where the operator has one.
    fn binop(&mut self, pc: usize, op: BinOp, l: Src, r: Src) {
        let has_fast = matches!(
            op,
            BinOp::Add
                | BinOp::Sub
                | BinOp::Mul
                | BinOp::Div
                | BinOp::Mod
                | BinOp::Exp
                | BinOp::Shl
                | BinOp::Shr
                | BinOp::UShr
                | BinOp::BitAnd
                | BinOp::BitOr
                | BinOp::BitXor
        ) || op.is_comparison();
        if !has_fast {
            self.slow(pc, None);
            return;
        }
        let lv = self.read(l);
        let rv = self.read(r);
        if let Some(negated) = equality(op) {
            let c = self.equal(pc, lv, rv, loose(op), None);
            let c = if negated {
                self.b.ins().bxor_imm_u(c, 1)
            } else {
                c
            };
            self.set_acc_bool(c);
            self.next(pc);
            return;
        }
        let both = self.numbers(&[lv, rv]);
        self.fast_or_slow(pc, both, None);
        let x = self.f64(lv);
        let y = self.f64(rv);
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                let z = match op {
                    BinOp::Add => self.b.ins().fadd(x, y),
                    BinOp::Sub => self.b.ins().fsub(x, y),
                    BinOp::Mul => self.b.ins().fmul(x, y),
                    _ => self.b.ins().fdiv(x, y),
                };
                self.set_acc_number(z);
                self.next(pc);
            }
            BinOp::Mod | BinOp::Exp => {
                // The runtime's own number semantics, called in place: it
                // reads no frame state, so nothing is flushed.
                let a = if op == BinOp::Mod {
                    Arith::Mod
                } else {
                    Arith::Exp
                };
                let f = self
                    .b
                    .ins()
                    .load(self.ptr, flags(), self.sh, self.off.s_arith);
                let k = self.b.ins().iconst(I32, a as i64);
                let call = self.b.ins().call_indirect(self.arith_sig, f, &[k, x, y]);
                let z = self.b.inst_results(call)[0];
                self.set_acc_number(z);
                self.next(pc);
            }
            BinOp::Shl | BinOp::Shr | BinOp::UShr | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                // ToInt32 / ToUint32 of a finite number below 2^63 in
                // magnitude: truncate, then keep the low 32 bits. Anything
                // else (NaN, infinities, huge values) takes the generic path.
                let limit = self.b.ins().f64const(9.223_372_036_854_776e18);
                let ax = self.b.ins().fabs(x);
                let ay = self.b.ins().fabs(y);
                let okx = self.b.ins().fcmp(FloatCC::LessThan, ax, limit);
                let oky = self.b.ins().fcmp(FloatCC::LessThan, ay, limit);
                let ok = self.b.ins().band(okx, oky);
                self.fast_or_slow(pc, Some(ok), None);
                let xi = self.b.ins().fcvt_to_sint_sat(I64, x);
                let yi = self.b.ins().fcvt_to_sint_sat(I64, y);
                let a = self.b.ins().ireduce(I32, xi);
                let c = self.b.ins().ireduce(I32, yi);
                let z = match op {
                    BinOp::Shl | BinOp::Shr | BinOp::UShr => {
                        let s = self.b.ins().band_imm_u(c, 31);
                        match op {
                            BinOp::Shl => self.b.ins().ishl(a, s),
                            BinOp::Shr => self.b.ins().sshr(a, s),
                            _ => self.b.ins().ushr(a, s),
                        }
                    }
                    BinOp::BitAnd => self.b.ins().band(a, c),
                    BinOp::BitOr => self.b.ins().bor(a, c),
                    _ => self.b.ins().bxor(a, c),
                };
                let f = if op == BinOp::UShr {
                    self.b.ins().fcvt_from_uint(F64, z)
                } else {
                    self.b.ins().fcvt_from_sint(F64, z)
                };
                self.set_acc_number(f);
                self.next(pc);
            }
            _ => {
                let c = self.b.ins().fcmp(float_cc(op), x, y);
                self.set_acc_bool(c);
                self.next(pc);
            }
        }
    }

    /// The fused compare-and-branch: jump when `l OP r` is `when`.
    fn compare_branch(&mut self, pc: usize, op: BinOp, l: Src, r: Src, off: i32, when: bool) {
        let to = (pc as i64 + off as i64) as usize;
        if !op.is_comparison() {
            self.slow(pc, Some(to));
            return;
        }
        let lv = self.read(l);
        let rv = self.read(r);
        if let Some(negated) = equality(op) {
            let c = self.equal(pc, lv, rv, loose(op), Some(to));
            let c = if negated == when {
                self.b.ins().bxor_imm_u(c, 1)
            } else {
                c
            };
            self.branch(pc, c, to);
            return;
        }
        let both = self.numbers(&[lv, rv]);
        self.fast_or_slow(pc, both, Some(to));
        let x = self.f64(lv);
        let y = self.f64(rv);
        let c = self.b.ins().fcmp(float_cc(op), x, y);
        let c = if when { c } else { self.b.ins().bxor_imm_u(c, 1) };
        self.branch(pc, c, to);
    }

    /// Whether `lv` and `rv` are equal, strictly or (`loose`) loosely,
    /// decided here for every pair that needs neither the heap nor a
    /// conversion: values of one type other than two strings or two
    /// BigInts that are not the same one, values of different types under
    /// `===`, and under `==` any pair with `null` or `undefined` in it.
    /// The other pairs run the instruction at `pc` by the runtime.
    fn equal(&mut self, pc: usize, lv: Val, rv: Val, loose: bool, to: Option<usize>) -> Ir {
        let x = self.f64(lv);
        let y = self.f64(rv);
        let numbers = self.b.ins().fcmp(FloatCC::Equal, x, y);
        if lv.tag.is_none() && rv.tag.is_none() {
            return numbers;
        }
        let tag = |s: &mut Self, v: Val| match v.tag {
            Some(t) => s.b.ins().band_imm_u(t, 0xff),
            None => s.b.ins().iconst(I64, TAG_NUMBER as i64),
        };
        let ta = tag(self, lv);
        let tb = tag(self, rv);
        let same = self.b.ins().icmp(IntCC::Equal, ta, tb);
        let is = |s: &mut Self, t: Ir, k: u8| s.b.ins().icmp_imm_u(IntCC::Equal, t, k as i64);
        let number = is(self, ta, TAG_NUMBER);
        let boolean = is(self, ta, TAG_BOOL);
        let object = is(self, ta, TAG_OBJECT);
        let symbol = is(self, ta, TAG_SYMBOL);
        let string = is(self, ta, TAG_STRING);
        let bigint = is(self, ta, TAG_BIGINT);
        // A boolean is one byte of the payload, an id four.
        let ba = self.b.ins().band_imm_u(lv.pay, 0xff);
        let bb = self.b.ins().band_imm_u(rv.pay, 0xff);
        let booleans = self.b.ins().icmp(IntCC::Equal, ba, bb);
        let ia = self.b.ins().band_imm_u(lv.pay, 0xffff_ffff);
        let ib = self.b.ins().band_imm_u(rv.pay, 0xffff_ffff);
        let ids = self.b.ins().icmp(IntCC::Equal, ia, ib);
        let heap = self.b.ins().bor(string, bigint);
        let reference = self.b.ins().bor(object, symbol);
        let by_id = self.b.ins().bor(reference, heap);
        // Of one type: undefined, null and the empty value have one value.
        let one = self.b.ins().iconst(types::I8, 1);
        let e = self.b.ins().select(by_id, ids, one);
        let e = self.b.ins().select(boolean, booleans, e);
        let e = self.b.ins().select(number, numbers, e);
        let equal = self.b.ins().band(same, e);
        // Two strings or two BigInts that are not the same one are
        // compared by the runtime.
        let differ = self.b.ins().bxor_imm_u(ids, 1);
        let by_content = self.b.ins().band(heap, differ);
        let mut undecided = self.b.ins().band(same, by_content);
        let mut result = equal;
        if loose {
            let nullish = |s: &mut Self, t: Ir| {
                let u = is(s, t, TAG_UNDEFINED);
                let n = is(s, t, TAG_NULL);
                s.b.ins().bor(u, n)
            };
            let na = nullish(self, ta);
            let nb = nullish(self, tb);
            let both = self.b.ins().band(na, nb);
            let either = self.b.ins().bor(na, nb);
            result = self.b.ins().bor(equal, both);
            // Different types, neither null nor undefined: a conversion.
            let different = self.b.ins().bxor_imm_u(same, 1);
            let neither = self.b.ins().bxor_imm_u(either, 1);
            let converts = self.b.ins().band(different, neither);
            undecided = self.b.ins().bor(undecided, converts);
        }
        let decided = self.b.ins().bxor_imm_u(undecided, 1);
        self.fast_or_slow(pc, Some(decided), to);
        result
    }

    /// `JumpIfTrue` / `JumpIfFalse`: ToBoolean inline for every type but
    /// strings and BigInts, which need the heap.
    fn truthy_branch(&mut self, pc: usize, off: i32, when: bool) {
        let to = (pc as i64 + off as i64) as usize;
        let (t, p) = self.acc();
        let tag = self.b.ins().band_imm_u(t, 0xff);
        let is_string = self.b.ins().icmp_imm_u(IntCC::Equal, tag, TAG_STRING as i64);
        let is_bigint = self.b.ins().icmp_imm_u(IntCC::Equal, tag, TAG_BIGINT as i64);
        let heap = self.b.ins().bor(is_string, is_bigint);
        let known = self.b.ins().bxor_imm_u(heap, 1);
        self.fast_or_slow(pc, Some(known), Some(to));
        let is_bool = self.b.ins().icmp_imm_u(IntCC::Equal, tag, TAG_BOOL as i64);
        let is_num = self.b.ins().icmp_imm_u(IntCC::Equal, tag, TAG_NUMBER as i64);
        let is_obj = self.b.ins().icmp_imm_u(IntCC::Equal, tag, TAG_OBJECT as i64);
        let is_sym = self.b.ins().icmp_imm_u(IntCC::Equal, tag, TAG_SYMBOL as i64);
        let always = self.b.ins().bor(is_obj, is_sym);
        let low = self.b.ins().band_imm_u(p, 0xff);
        let b_true = self.b.ins().icmp_imm_u(IntCC::NotEqual, low, 0);
        let x = self.b.ins().bitcast(F64, MemFlagsData::new(), p);
        let zero = self.b.ins().f64const(0.0);
        let n_true = self.b.ins().fcmp(FloatCC::OrderedNotEqual, x, zero);
        let v = self.b.ins().select(is_num, n_true, always);
        let v = self.b.ins().select(is_bool, b_true, v);
        let c = if when { v } else { self.b.ins().bxor_imm_u(v, 1) };
        self.branch(pc, c, to);
    }

    /// A jump on the accumulator's tag alone.
    fn tag_branch(&mut self, pc: usize, off: i32, tags: &[u8], when: bool) {
        let to = (pc as i64 + off as i64) as usize;
        let (t, _) = self.acc();
        let mut c = None;
        for &tag in tags {
            let e = self.is_tag(t, tag);
            c = Some(match c {
                Some(a) => self.b.ins().bor(a, e),
                None => e,
            });
        }
        let c = c.unwrap();
        let c = if when { c } else { self.b.ins().bxor_imm_u(c, 1) };
        self.branch(pc, c, to);
    }

    // ---- properties -------------------------------------------------------
    //
    // The inline-cache hit paths (TASK-101.3.2). They read the heap through
    // the arena base in the context, loaded afresh by each instruction and
    // never live across a call out (the arena may move, or a collection
    // run, under one); a value read from the heap goes straight to the
    // accumulator, which is flushed to the context before any call. Every
    // guard that fails jumps to the instruction's generic path, which
    // repeats the interpreter's own lookup, so the tests below only have to
    // be no more permissive than the interpreter's (`interp/ic.rs`).

    /// A cold block running the instruction at `pc` generically; the
    /// caller fills it with [`Self::fill_slow`] once it has branched to it.
    fn slow_block(&mut self) -> Block {
        let blk = self.b.create_block();
        self.b.set_cold_block(blk);
        blk
    }

    fn fill_slow(&mut self, blk: Block, pc: usize) {
        self.b.switch_to_block(blk);
        self.slow(pc, None);
    }

    /// Continue in a fresh block when `cond`, else at `otherwise`.
    fn check(&mut self, cond: Ir, otherwise: Block) {
        let yes = self.b.create_block();
        self.b.ins().brif(cond, yes, &[], otherwise, &[]);
        self.b.switch_to_block(yes);
    }

    fn load_u8(&mut self, base: Ir, off: u32) -> Ir {
        self.b.ins().uload8(I64, flags(), base, off as i32)
    }

    fn load_u32(&mut self, base: Ir, off: u32) -> Ir {
        self.b.ins().uload32(flags(), base, off as i32)
    }

    fn load_word(&mut self, base: Ir, off: u32) -> Ir {
        let w = self.b.ins().load(self.ptr, flags(), base, off as i32);
        if self.ptr == I64 {
            w
        } else {
            self.b.ins().uextend(I64, w)
        }
    }

    /// The address of object `id` (a 64-bit word holding a `u32` id).
    fn object(&mut self, h: &HeapLayout, id: Ir) -> Ir {
        let base = self
            .b
            .ins()
            .load(self.ptr, flags(), self.sh, self.off.s_objects);
        let id = if self.ptr == I64 {
            id
        } else {
            self.b.ins().ireduce(self.ptr, id)
        };
        let at = self.b.ins().imul_imm_u(id, h.obj_stride as i64);
        let o = self.b.ins().iadd(base, at);
        self.b.ins().iadd_imm_u(o, h.obj_offset as i64)
    }

    /// The object in register `r`, or a jump to `miss` for anything else.
    fn object_in(&mut self, h: &HeapLayout, r: u16, miss: Block) -> Ir {
        let (t, p) = self.load_reg(r);
        let is = self.is_tag(t, TAG_OBJECT);
        self.check(is, miss);
        let id = self.b.ins().band_imm_u(p, 0xffff_ffff);
        self.object(h, id)
    }

    /// `bits` has the bit for the class tag `class` set.
    fn class_in(&mut self, class: Ir, bits: u64) -> Ir {
        let b = self.b.ins().iconst(I64, bits as i64);
        let s = self.b.ins().ushr(b, class);
        let s = self.b.ins().band_imm_u(s, 1);
        self.b.ins().icmp_imm_u(IntCC::NotEqual, s, 0)
    }

    /// Whether `o` (class tag `class`) is a mapped arguments object.
    fn mapped_arguments(&mut self, h: &HeapLayout, o: Ir, class: Ir) -> Ir {
        let is_args = self.b.ins().icmp_imm_u(IntCC::Equal, class, h.class_arguments as i64);
        let w = self.load_word(o, h.arguments_mapped);
        let mapped = self.b.ins().icmp_imm_u(IntCC::NotEqual, w, 0);
        self.b.ins().band(is_args, mapped)
    }

    /// The address of slot `slot` (a 64-bit word) of object `o`, or a jump
    /// to `miss` when the object has no such slot.
    fn slot_addr(&mut self, h: &HeapLayout, o: Ir, slot: Ir, miss: Block) -> Ir {
        let inline = self.b.create_block();
        let over = self.b.create_block();
        let done = self.b.create_block();
        self.b.append_block_param(done, self.ptr);
        let in_inline = self.b.ins().icmp_imm_u(IntCC::UnsignedLessThan, slot, h.inline_slots as i64);
        self.b.ins().brif(in_inline, inline, &[], over, &[]);

        self.b.switch_to_block(inline);
        let s = self.narrow(slot);
        let at = self.b.ins().imul_imm_u(s, SIZE as i64);
        let a = self.b.ins().iadd(o, at);
        let a = self.b.ins().iadd_imm_u(a, h.inline as i64);
        self.b.ins().jump(done, &[a.into()]);

        self.b.switch_to_block(over);
        let i = self.b.ins().iadd_imm_s(slot, -(h.inline_slots as i64));
        let len = self.load_word(o, h.overflow_len);
        let ok = self.b.ins().icmp(IntCC::UnsignedLessThan, i, len);
        self.check(ok, miss);
        let buf = self.b.ins().load(self.ptr, flags(), o, h.overflow_ptr as i32);
        let i = self.narrow(i);
        let at = self.b.ins().imul_imm_u(i, SIZE as i64);
        let a = self.b.ins().iadd(buf, at);
        self.b.ins().jump(done, &[a.into()]);

        self.b.switch_to_block(done);
        self.b.block_params(done)[0]
    }

    fn narrow(&mut self, w: Ir) -> Ir {
        if self.ptr == I64 {
            w
        } else {
            self.b.ins().ireduce(self.ptr, w)
        }
    }

    /// Load the data property in the slot at `a` into the accumulator and
    /// go on, or jump to `miss` for an accessor.
    fn read_data_slot(&mut self, pc: usize, a: Ir, miss: Block) {
        let t = self.b.ins().load(I64, flags(), a, TAG_OFFSET as i32);
        let tag = self.b.ins().band_imm_u(t, 0xff);
        let data = self.b.ins().icmp_imm_u(IntCC::UnsignedLessThanOrEqual, tag, TAG_EMPTY as i64);
        self.check(data, miss);
        let p = self.b.ins().load(I64, flags(), a, PAYLOAD_OFFSET as i32);
        self.set_acc(t, p);
        self.next(pc);
    }

    /// The address of entry `k` of inline-cache site `site`.
    fn ic_entry(&mut self, site: u16, k: usize) -> Ir {
        let table = self.b.ins().load(self.ptr, flags(), self.ctx, self.off.ic);
        let at = site as usize * std::mem::size_of::<IcSite>() + k * std::mem::size_of::<IcEntry>();
        self.b.ins().iadd_imm_u(table, at as i64)
    }

    /// `acc = obj.name` through site `ic`: own and prototype data
    /// properties and array lengths, as `Vm::ic_get` finds them.
    fn get_named(&mut self, pc: usize, obj: u16, ic: u16) {
        let Some(h) = self.inline_site(ic) else {
            self.slow(pc, None);
            return;
        };
        let n = self.unroll[ic as usize] as usize;
        let miss = self.slow_block();
        let o = self.object_in(&h, obj, miss);
        let class = self.load_u8(o, h.class);
        let dict = self.load_word(o, h.dict);
        let not_dict = self.b.ins().icmp_imm_u(IntCC::Equal, dict, 0);
        self.check(not_dict, miss);
        let excluded = self.class_in(class, h.not_receiver);
        let mapped = self.mapped_arguments(&h, o, class);
        let bad = self.b.ins().bor(excluded, mapped);
        let ok = self.b.ins().bxor_imm_u(bad, 1);
        self.check(ok, miss);
        let shape = self.load_u32(o, h.shape);
        let is_array = self.b.ins().icmp_imm_u(IntCC::Equal, class, h.class_array as i64);
        let is_string = self.b.ins().icmp_imm_u(IntCC::Equal, class, h.class_string as i64);
        let exotic = self.b.ins().bor(is_array, is_string);

        let mut cur = self.b.create_block();
        self.b.ins().jump(cur, &[]);
        for k in 0..n.min(IC_ENTRIES) {
            let next = if k + 1 < n { self.b.create_block() } else { miss };
            self.b.switch_to_block(cur);
            let e = self.ic_entry(ic, k);
            let kind = self.load_u8(e, std::mem::offset_of!(IcEntry, kind) as u32);
            let fl = self.load_u8(e, std::mem::offset_of!(IcEntry, flags) as u32);
            // A `length` entry never applies to a length-exotic receiver.
            let len_key = self.b.ins().band_imm_u(fl, IcEntry::FLAG_LENGTH as i64);
            let len_key = self.b.ins().icmp_imm_u(IntCC::NotEqual, len_key, 0);
            let skip = self.b.ins().band(len_key, exotic);
            let applies = self.b.ins().bxor_imm_u(skip, 1);
            self.check(applies, next);
            // Array length.
            let alen = self.b.create_block();
            let other = self.b.create_block();
            let is_alen = self.b.ins().icmp_imm_u(IntCC::Equal, kind, IcEntry::KIND_ARRAY_LENGTH as i64);
            self.b.ins().brif(is_alen, alen, &[], other, &[]);
            self.b.switch_to_block(alen);
            self.check(is_array, next);
            let l = self.load_u32(o, h.array_length);
            let f = self.b.ins().fcvt_from_uint(F64, l);
            self.set_acc_number(f);
            self.next(pc);
            // No entry, or no inline case at all, from here on.
            self.b.switch_to_block(other);
            let empty = self.b.ins().icmp_imm_u(IntCC::Equal, kind, IcEntry::KIND_EMPTY as i64);
            let mega = self.b.ins().icmp_imm_u(IntCC::Equal, kind, IcEntry::KIND_MEGAMORPHIC as i64);
            let stop = self.b.ins().bor(empty, mega);
            let go = self.b.ins().bxor_imm_u(stop, 1);
            self.check(go, miss);
            let eshape = self.load_u32(e, std::mem::offset_of!(IcEntry, shape) as u32);
            let same = self.b.ins().icmp(IntCC::Equal, shape, eshape);
            self.check(same, next);
            let slot = self.load_u32(e, std::mem::offset_of!(IcEntry, slot) as u32);
            // Own data.
            let own = self.b.create_block();
            let not_own = self.b.create_block();
            let is_own = self.b.ins().icmp_imm_u(IntCC::Equal, kind, IcEntry::KIND_OWN_DATA as i64);
            self.b.ins().brif(is_own, own, &[], not_own, &[]);
            self.b.switch_to_block(own);
            let a = self.slot_addr(&h, o, slot, miss);
            self.read_data_slot(pc, a, miss);
            // Prototype data, in the epoch it was recorded in. Any other
            // kind for this shape (an accessor) is the generic path's.
            self.b.switch_to_block(not_own);
            let is_proto = self.b.ins().icmp_imm_u(IntCC::Equal, kind, IcEntry::KIND_PROTO_DATA as i64);
            self.check(is_proto, miss);
            let proto = self.load_u32(o, h.proto);
            let eproto = self.load_u32(e, std::mem::offset_of!(IcEntry, proto) as u32);
            let epoch = self
                .b
                .ins()
                .uload32(flags(), self.sh, self.off.s_proto_epoch);
            let eepoch = self.load_u32(e, std::mem::offset_of!(IcEntry, epoch) as u32);
            let p_ok = self.b.ins().icmp(IntCC::Equal, proto, eproto);
            let e_ok = self.b.ins().icmp(IntCC::Equal, epoch, eepoch);
            let ok = self.b.ins().band(p_ok, e_ok);
            self.check(ok, next);
            let holder = self.load_u32(e, std::mem::offset_of!(IcEntry, holder) as u32);
            let ho = self.object(&h, holder);
            let a = self.slot_addr(&h, ho, slot, miss);
            self.read_data_slot(pc, a, miss);
            cur = next;
        }
        self.fill_slow(miss, pc);
    }

    /// `obj.name = acc` through site `ic`: an own writable data property,
    /// as `Vm::ic_set` writes it.
    fn set_named(&mut self, pc: usize, obj: u16, ic: u16) {
        let Some(h) = self.inline_site(ic) else {
            self.slow(pc, None);
            return;
        };
        let n = self.unroll[ic as usize] as usize;
        let miss = self.slow_block();
        let o = self.object_in(&h, obj, miss);
        let class = self.load_u8(o, h.class);
        let dict = self.load_word(o, h.dict);
        let not_dict = self.b.ins().icmp_imm_u(IntCC::Equal, dict, 0);
        self.check(not_dict, miss);
        let excluded = self.class_in(class, h.not_receiver);
        let mapped = self.mapped_arguments(&h, o, class);
        let bad = self.b.ins().bor(excluded, mapped);
        let ok = self.b.ins().bxor_imm_u(bad, 1);
        self.check(ok, miss);
        let shape = self.load_u32(o, h.shape);
        let mut cur = self.b.create_block();
        self.b.ins().jump(cur, &[]);
        for k in 0..n.min(IC_ENTRIES) {
            let next = if k + 1 < n { self.b.create_block() } else { miss };
            self.b.switch_to_block(cur);
            let e = self.ic_entry(ic, k);
            let kind = self.load_u8(e, std::mem::offset_of!(IcEntry, kind) as u32);
            let empty = self.b.ins().icmp_imm_u(IntCC::Equal, kind, IcEntry::KIND_EMPTY as i64);
            let mega = self.b.ins().icmp_imm_u(IntCC::Equal, kind, IcEntry::KIND_MEGAMORPHIC as i64);
            let stop = self.b.ins().bor(empty, mega);
            let go = self.b.ins().bxor_imm_u(stop, 1);
            self.check(go, miss);
            let eshape = self.load_u32(e, std::mem::offset_of!(IcEntry, shape) as u32);
            let same = self.b.ins().icmp(IntCC::Equal, shape, eshape);
            let is_own = self.b.ins().icmp_imm_u(IntCC::Equal, kind, IcEntry::KIND_OWN_DATA as i64);
            let hit = self.b.ins().band(same, is_own);
            // The same shape under another kind (an add, an accessor) is
            // the generic path's; another shape, the next entry's.
            let other = self.b.create_block();
            let write = self.b.create_block();
            self.b.ins().brif(hit, write, &[], other, &[]);
            self.b.switch_to_block(other);
            self.b.ins().brif(same, miss, &[], next, &[]);
            self.b.switch_to_block(write);
            let slot = self.load_u32(e, std::mem::offset_of!(IcEntry, slot) as u32);
            let a = self.slot_addr(&h, o, slot, miss);
            let t = self.b.ins().load(I64, flags(), a, TAG_OFFSET as i32);
            let tag = self.b.ins().band_imm_u(t, 0xff);
            let data = self.b.ins().icmp_imm_u(IntCC::UnsignedLessThanOrEqual, tag, TAG_EMPTY as i64);
            self.check(data, miss);
            let (vt, vp) = self.acc();
            self.store_value(a, 0, vt, vp);
            self.next(pc);
            cur = next;
        }
        self.fill_slow(miss, pc);
    }

    /// The heap layout, when site `ic` is one compiled code tests inline.
    fn inline_site(&self, ic: u16) -> Option<HeapLayout> {
        let h = self.heap?;
        ((ic as usize) < self.unroll.len() && self.unroll[ic as usize] > 0).then_some(h)
    }

    /// The address of the present dense element `obj[key]`, or a jump to
    /// `miss`, as `Vm::keyed_get_fast` and `keyed_set_fast` find it.
    fn dense_element(&mut self, h: &HeapLayout, obj: u16, key: u16, miss: Block) -> Ir {
        let (kt, kp) = self.load_reg(key);
        let is_num = self.is_tag(kt, TAG_NUMBER);
        self.check(is_num, miss);
        let o = self.object_in(h, obj, miss);
        // An array index: a number that survives the round trip through
        // u32 (-0 names index 0), other than 2^32 - 1.
        let x = self.b.ins().bitcast(F64, MemFlagsData::new(), kp);
        let i = self.b.ins().fcvt_to_uint_sat(I32, x);
        let back = self.b.ins().fcvt_from_uint(F64, i);
        let exact = self.b.ins().fcmp(FloatCC::Equal, back, x);
        let not_max = self.b.ins().icmp_imm_u(IntCC::NotEqual, i, u32::MAX as i64);
        let index = self.b.ins().band(exact, not_max);
        self.check(index, miss);
        let class = self.load_u8(o, h.class);
        let excluded = self.class_in(class, h.not_elements);
        let mapped = self.mapped_arguments(h, o, class);
        let bad = self.b.ins().bor(excluded, mapped);
        let ok = self.b.ins().bxor_imm_u(bad, 1);
        self.check(ok, miss);
        // Dense: a non-null buffer and a capacity that is not the sparse
        // case's niche (see `HeapLayout`).
        let buf = self.load_word(o, h.elements_ptr);
        let has = self.b.ins().icmp_imm_u(IntCC::NotEqual, buf, 0);
        self.check(has, miss);
        let cap = self.load_word(o, h.elements_cap);
        let dense = self.b.ins().icmp_imm_s(IntCC::SignedGreaterThanOrEqual, cap, 0);
        self.check(dense, miss);
        let len = self.load_word(o, h.elements_len);
        let i = self.b.ins().uextend(I64, i);
        let inb = self.b.ins().icmp(IntCC::UnsignedLessThan, i, len);
        self.check(inb, miss);
        let at = self.b.ins().imul_imm_u(i, SIZE as i64);
        let a = self.b.ins().iadd(buf, at);
        let a = self.narrow(a);
        let t = self.b.ins().load(I64, flags(), a, TAG_OFFSET as i32);
        let tag = self.b.ins().band_imm_u(t, 0xff);
        let present = self.b.ins().icmp_imm_u(IntCC::NotEqual, tag, TAG_EMPTY as i64);
        self.check(present, miss);
        a
    }

    fn get_keyed(&mut self, pc: usize, obj: u16, key: u16) {
        let Some(h) = self.heap else {
            self.slow(pc, None);
            return;
        };
        let miss = self.slow_block();
        let a = self.dense_element(&h, obj, key, miss);
        let t = self.b.ins().load(I64, flags(), a, TAG_OFFSET as i32);
        let p = self.b.ins().load(I64, flags(), a, PAYLOAD_OFFSET as i32);
        self.set_acc(t, p);
        self.next(pc);
        self.fill_slow(miss, pc);
    }

    fn set_keyed(&mut self, pc: usize, obj: u16, key: u16) {
        let Some(h) = self.heap else {
            self.slow(pc, None);
            return;
        };
        let miss = self.slow_block();
        let a = self.dense_element(&h, obj, key, miss);
        let (vt, vp) = self.acc();
        self.b.ins().store(flags(), vt, a, TAG_OFFSET as i32);
        self.b.ins().store(flags(), vp, a, PAYLOAD_OFFSET as i32);
        self.next(pc);
        self.fill_slow(miss, pc);
    }

    // ---- calls ----------------------------------------------------------------
    //
    // A `Call` of a function the runtime's table has is made in machine
    // code (TASK-101.3.3): the callee's frame is pushed on the runtime's own
    // frame stack, its machine code is called on the native stack, and the
    // frame is popped when it returns. Nothing of the calling function is
    // outside its interpreter frame meanwhile but what the call's safe
    // point lists (the same point a call through the runtime uses), and the
    // callee keeps nothing in machine state at all, so the collector and a
    // debugger see every frame as they do under the interpreter. Whatever
    // the fast path cannot do, the runtime does: every guard that fails
    // makes the call through `Runtime::call`, from the same state.
    //
    // The sequence is long, so there is one copy of it, the call thunk
    // ([`compile_thunk`]), which every call site calls with the
    // instruction's operands: a site is the safe point and a call.

    /// The call through the runtime, after the call's safe point.
    fn call_runtime(&mut self, pc: usize) {
        let f = self
            .b
            .ins()
            .load(self.ptr, flags(), self.sh, self.off.s_call);
        let a = self.b.ins().iconst(I32, pc as i64);
        let call = self.b.ins().call_indirect(self.slow_sig, f, &[self.ctx, a]);
        let r = self.b.inst_results(call)[0];
        self.continue_at(r, pc, &[pc + 1]);
    }

    fn call(&mut self, pc: usize, callee: u16, this: u16, args: u16, argc: u16) {
        // The call's safe point, whichever way the call is made: the
        // registers the call reads are in the file, the others this
        // function holds are spilled and listed.
        self.flush_acc();
        let reads = access(&self.code.instrs[pc]);
        let next = vec![pc as u32 + 1];
        self.point(pc, PointKind::SafePoint { next }, Some(&reads));
        if self.thunk == 0 || !self.calls.is_some_and(|c| c.direct) {
            self.call_runtime(pc);
            return;
        }
        // The thunk makes the call or has the runtime make it, and answers
        // as a call out does: the pc to continue at, with the accumulator
        // and the register window in the context, or an exit code.
        let f = self.b.ins().iconst(I64, self.thunk as i64);
        let ops = (callee as u64) | (this as u64) << 16 | (args as u64) << 32 | (argc as u64) << 48;
        let ops = self.b.ins().iconst(I64, ops as i64);
        let a = self.b.ins().iconst(I32, pc as i64);
        let call = self
            .b
            .ins()
            .call_indirect(self.thunk_sig, f, &[self.ctx, self.sh, ops, a]);
        let r = self.b.inst_results(call)[0];
        self.continue_at(r, pc, &[pc + 1]);
    }

    /// Copy `size` bytes (a multiple of 8) from `src` to `dst`.
    fn copy_bytes(&mut self, dst: Ir, src: Ir, size: u32) {
        let unaligned = MemFlagsData::new().with_notrap();
        let mut at = 0i32;
        while at as u32 + 16 <= size {
            let v = self.b.ins().load(I64X2, unaligned, src, at);
            self.b.ins().store(unaligned, v, dst, at);
            at += 16;
        }
        while at as u32 + 8 <= size {
            let v = self.b.ins().load(I64, flags(), src, at);
            self.b.ins().store(flags(), v, dst, at);
            at += 8;
        }
    }

    /// A field of `size` bytes (4 or 8) at `base + off`.
    fn load_field(&mut self, base: Ir, off: u32, size: u32) -> Ir {
        let ty = if size == 4 { I32 } else { I64 };
        self.b
            .ins()
            .load(ty, MemFlagsData::new().with_notrap(), base, off as i32)
    }

    /// Register `r` (a 64-bit index) of the window at `regs`.
    fn register(&mut self, regs: Ir, r: Ir) -> (Ir, Ir) {
        let at = self.b.ins().imul_imm_u(r, SIZE as i64);
        let a = self.b.ins().iadd(regs, at);
        let t = self.b.ins().load(I64, flags(), a, TAG_OFFSET as i32);
        let p = self.b.ins().load(I64, flags(), a, PAYLOAD_OFFSET as i32);
        (t, p)
    }

    /// Return what the runtime entry `entry` (two arguments) answers.
    fn thunk_tail(&mut self, entry: i32, arg: Ir) {
        let f = self.b.ins().load(self.ptr, flags(), self.sh, entry);
        let call = self
            .b
            .ins()
            .call_indirect(self.slow_sig, f, &[self.ctx, arg]);
        let r = self.b.inst_results(call)[0];
        self.b.ins().return_(&[r]);
    }

    /// Return what `Runtime::returned` answers for the call at `pc`, the
    /// callee's value being `(t, p)`.
    fn thunk_returned(&mut self, pc: Ir, t: Ir, p: Ir, popped: bool) {
        self.store_value(self.ctx, self.off.acc, t, p);
        let f = self
            .b
            .ins()
            .load(self.ptr, flags(), self.sh, self.off.s_ret);
        let k = self.b.ins().iconst(I32, popped as i64);
        let call = self
            .b
            .ins()
            .call_indirect(self.poll_sig, f, &[self.ctx, pc, k]);
        let r = self.b.inst_results(call)[0];
        self.b.ins().return_(&[r]);
    }

    /// The call thunk's body: the `Call` at `pc` (an `i32`) whose operands
    /// are packed in `ops` (callee, `this` and first argument registers and
    /// the argument count, sixteen bits each from the low end), for the
    /// frame whose context is this function's.
    fn thunk(&mut self, h: &HeapLayout, c: &CallLayout, slot: StackSlot, ops: Ir, pc: Ir) {
        let size = c.frame_size as i64;
        let miss = self.slow_block();
        let callee = self.b.ins().band_imm_u(ops, 0xffff);
        let this = self.b.ins().ushr_imm_u(ops, 16);
        let this = self.b.ins().band_imm_u(this, 0xffff);
        let args = self.b.ins().ushr_imm_u(ops, 32);
        let args = self.b.ins().band_imm_u(args, 0xffff);
        let argc = self.b.ins().ushr_imm_u(ops, 48);
        let mine = self.b.use_var(self.regs);

        // The callee: a function object whose code block the runtime's
        // table has (an empty entry's key equals none), for the function's
        // realm.
        let (ft, fp) = self.register(mine, callee);
        let is_obj = self.is_tag(ft, TAG_OBJECT);
        self.check(is_obj, miss);
        let id = self.b.ins().band_imm_u(fp, 0xffff_ffff);
        let o = self.object(h, id);
        let class = self.load_u8(o, h.class);
        let is_fn = self
            .b
            .ins()
            .icmp_imm_u(IntCC::Equal, class, c.class_function as i64);
        self.check(is_fn, miss);
        let fd = self.b.ins().load(I64, flags(), o, c.function_data as i32);
        let code_w = self.b.ins().load(I64, flags(), fd, c.fn_code as i32);
        let table = self.b.ins().load(I64, flags(), self.sh, self.off.s_callees);
        let mult = self.b.ins().iconst(I64, CALLEE_HASH as i64);
        let hash = self.b.ins().imul(code_w, mult);
        let bucket = self.b.ins().ushr_imm_u(hash, (64 - CALLEE_BITS) as i64);
        let at = self
            .b
            .ins()
            .imul_imm_u(bucket, CALLEE.size as i64 * CALLEE_WAYS as i64);
        let first = self.b.ins().iadd(table, at);
        let found = self.b.create_block();
        self.b.append_block_param(found, I64);
        for way in 0..CALLEE_WAYS {
            let e = self
                .b
                .ins()
                .iadd_imm_u(first, way as i64 * CALLEE.size as i64);
            let key = self.b.ins().load(I64, flags(), e, CALLEE.code);
            let same = self.b.ins().icmp(IntCC::Equal, code_w, key);
            if way + 1 < CALLEE_WAYS {
                let next = self.b.create_block();
                self.b.ins().brif(same, found, &[e.into()], next, &[]);
                self.b.switch_to_block(next);
            } else {
                self.b.ins().brif(same, found, &[e.into()], miss, &[]);
            }
        }
        self.b.switch_to_block(found);
        let s = self.b.block_params(found)[0];
        let realm = self.b.ins().load(I32, flags(), fd, c.fn_realm as i32);
        let entry_realm = self.b.ins().load(I32, flags(), s, CALLEE.realm);
        let same_realm = self.b.ins().icmp(IntCC::Equal, realm, entry_realm);
        self.check(same_realm, miss);

        // No poll is due at the function's entry (ADR 0019 J7), no safe
        // point is wanted, and the native stack is above the runtime's
        // floor for one more machine frame.
        let cnt = self.b.ins().load(I32, flags(), self.sh, self.off.s_counter);
        let cnt = self.b.ins().iadd_imm_u(cnt, 1);
        let low = self.b.ins().band_imm_u(cnt, 0xfff);
        let not_due = self.b.ins().icmp_imm_u(IntCC::NotEqual, low, 0);
        let attend = self.b.ins().load(I32, flags(), self.sh, self.off.s_attend);
        let calm = self.b.ins().icmp_imm_u(IntCC::Equal, attend, 0);
        let sp = self.b.ins().get_stack_pointer(I64);
        let floor = self.b.ins().load(I64, flags(), self.sh, self.off.s_floor);
        let shallow = self.b.ins().icmp(IntCC::UnsignedGreaterThan, sp, floor);
        let ok = self.b.ins().band(not_due, calm);
        let ok = self.b.ins().band(ok, shallow);
        self.check(ok, miss);

        // `this`: as passed, or (a function that takes the global object
        // for a missing `this`) the global object the frame image holds. A
        // primitive such a function would box is the runtime's.
        let (tt, tp) = self.register(mine, this);
        let mode = self.load_u8(fd, c.fn_this_mode);
        let global = self
            .b
            .ins()
            .icmp_imm_u(IntCC::Equal, mode, c.this_mode_global as i64);
        let ttag = self.b.ins().band_imm_u(tt, 0xff);
        let t_obj = self
            .b
            .ins()
            .icmp_imm_u(IntCC::Equal, ttag, TAG_OBJECT as i64);
        let t_undef = self
            .b
            .ins()
            .icmp_imm_u(IntCC::Equal, ttag, TAG_UNDEFINED as i64);
        let t_null = self.b.ins().icmp_imm_u(IntCC::Equal, ttag, TAG_NULL as i64);
        let nullish = self.b.ins().bor(t_undef, t_null);
        let as_passed = self.b.ins().bxor_imm_u(global, 1);
        let ok = self.b.ins().bor(as_passed, t_obj);
        let ok = self.b.ins().bor(ok, nullish);
        self.check(ok, miss);
        let image = self.b.ins().load(I64, flags(), s, CALLEE.template);
        let gt = self
            .b
            .ins()
            .load(I64, flags(), image, c.frame_this as i32 + TAG_OFFSET as i32);
        let gp = self.b.ins().load(
            I64,
            flags(),
            image,
            c.frame_this as i32 + PAYLOAD_OFFSET as i32,
        );
        let subst = self.b.ins().band(global, nullish);
        let tt = self.b.ins().select(subst, gt, tt);
        let tp = self.b.ins().select(subst, gp, tp);

        // Room for the frame and its registers without growing either
        // vector, and the callee's inline caches of the current epoch (what
        // the runtime checks when it pushes a frame).
        let fv = self.b.ins().load(I64, flags(), self.sh, self.off.s_frames);
        let fl = self.b.ins().load(I64, flags(), fv, c.frames.len as i32);
        let fcap = self.b.ins().load(I64, flags(), fv, c.frames.cap as i32);
        let room = self.b.ins().icmp(IntCC::UnsignedLessThan, fl, fcap);
        let under = self
            .b
            .ins()
            .icmp_imm_u(IntCC::UnsignedLessThan, fl, c.max_frames as i64);
        let rv = self
            .b
            .ins()
            .load(I64, flags(), self.sh, self.off.s_registers);
        let rl = self.b.ins().load(I64, flags(), rv, c.registers.len as i32);
        let rcap = self.b.ins().load(I64, flags(), rv, c.registers.cap as i32);
        let n = self.b.ins().uload32(flags(), s, CALLEE.registers);
        let nl = self.b.ins().iadd(rl, n);
        let fits = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, nl, rcap);
        let epoch_at = self.b.ins().load(I64, flags(), s, CALLEE.shape_epoch);
        let epoch = self.b.ins().load(I64, flags(), epoch_at, 0);
        let now = self
            .b
            .ins()
            .load(I64, flags(), self.sh, self.off.s_shape_epoch);
        let fresh = self.b.ins().icmp(IntCC::Equal, epoch, now);
        let ok = self.b.ins().band(room, under);
        let ok = self.b.ins().band(ok, fits);
        let ok = self.b.ins().band(ok, fresh);
        self.check(ok, miss);

        // ---- the call is made here from now on ----
        self.b
            .ins()
            .store(flags(), cnt, self.sh, self.off.s_counter);
        let made = self
            .b
            .ins()
            .load(I64, flags(), self.sh, self.off.s_direct_calls);
        let made = self.b.ins().iadd_imm_u(made, 1);
        self.b
            .ins()
            .store(flags(), made, self.sh, self.off.s_direct_calls);

        // The caller's frame, written back as before any call.
        let frames = self.b.ins().load(I64, flags(), fv, c.frames.ptr as i32);
        let at = self.b.ins().imul_imm_u(fl, size);
        let nf = self.b.ins().iadd(frames, at);
        let cf = self.b.ins().iadd_imm_s(nf, -size);
        let after = self.b.ins().uextend(I64, pc);
        let after = self.b.ins().iadd_imm_u(after, 1);
        self.b.ins().store(flags(), after, cf, c.frame_pc as i32);
        let (at, ap) = self.acc();
        self.store_value(cf, c.frame_acc as i32, at, ap);

        // The callee's frame: the image, then what differs per call. The
        // image's reference to the code block is taken by counting it.
        self.copy_bytes(nf, image, c.frame_size);
        self.b.ins().store(flags(), rl, nf, c.frame_base as i32);
        self.store_value(nf, c.frame_this as i32, tt, tp);
        self.b.ins().store(flags(), argc, nf, c.frame_argc as i32);
        let id32 = self.b.ins().ireduce(I32, id);
        self.b.ins().store(flags(), id32, nf, c.frame_callee as i32);
        let unaligned = MemFlagsData::new().with_notrap();
        let env = self.load_field(fd, c.fn_env, c.env_size);
        self.b.ins().store(unaligned, env, nf, c.frame_env as i32);
        self.b
            .ins()
            .store(unaligned, env, nf, c.frame_base_env as i32);
        let module = self.load_field(fd, c.fn_module, c.module_size);
        self.b
            .ins()
            .store(unaligned, module, nf, c.frame_module as i32);
        let count = self.b.ins().load(I64, flags(), code_w, 0);
        let count = self.b.ins().iadd_imm_u(count, 1);
        self.b.ins().store(flags(), count, code_w, 0);
        let pushed = self.b.ins().iadd_imm_u(fl, 1);
        self.b.ins().store(flags(), pushed, fv, c.frames.len as i32);

        // Its registers: undefined, then the arguments it takes (as many
        // as it has parameter registers for).
        let file = self.b.ins().load(I64, flags(), rv, c.registers.ptr as i32);
        let at = self.b.ins().imul_imm_u(rl, SIZE as i64);
        let window = self.b.ins().iadd(file, at);
        let zero = self.b.ins().iconst(I64, 0);
        const { assert!(TAG_UNDEFINED == 0) };
        let undefined = self.b.ins().scalar_to_vector(I64X2, zero);
        let head = self.b.create_block();
        let body = self.b.create_block();
        let cleared = self.b.create_block();
        self.b.append_block_param(head, I64);
        self.b.ins().jump(head, &[zero.into()]);
        self.b.switch_to_block(head);
        let i = self.b.block_params(head)[0];
        let more = self.b.ins().icmp(IntCC::UnsignedLessThan, i, n);
        self.b.ins().brif(more, body, &[], cleared, &[]);
        self.b.switch_to_block(body);
        let at = self.b.ins().imul_imm_u(i, SIZE as i64);
        let a = self.b.ins().iadd(window, at);
        self.b.ins().store(unaligned, undefined, a, 0);
        let i1 = self.b.ins().iadd_imm_u(i, 1);
        self.b.ins().jump(head, &[i1.into()]);
        self.b.switch_to_block(cleared);
        self.b.ins().store(flags(), nl, rv, c.registers.len as i32);
        let params = self.b.ins().uload32(flags(), s, CALLEE.param_regs);
        let take = self.b.ins().umin(params, argc);
        let first = self.b.ins().uload32(flags(), s, CALLEE.param_base);
        let at = self.b.ins().imul_imm_u(first, SIZE as i64);
        let dst = self.b.ins().iadd(window, at);
        let at = self.b.ins().imul_imm_u(args, SIZE as i64);
        let src = self.b.ins().iadd(mine, at);
        let head = self.b.create_block();
        let body = self.b.create_block();
        let copied = self.b.create_block();
        self.b.append_block_param(head, I64);
        self.b.ins().jump(head, &[zero.into()]);
        self.b.switch_to_block(head);
        let j = self.b.block_params(head)[0];
        let more = self.b.ins().icmp(IntCC::UnsignedLessThan, j, take);
        self.b.ins().brif(more, body, &[], copied, &[]);
        self.b.switch_to_block(body);
        let at = self.b.ins().imul_imm_u(j, SIZE as i64);
        let from = self.b.ins().iadd(src, at);
        let to = self.b.ins().iadd(dst, at);
        let v = self.b.ins().load(I64X2, unaligned, from, 0);
        self.b.ins().store(unaligned, v, to, 0);
        let j1 = self.b.ins().iadd_imm_u(j, 1);
        self.b.ins().jump(head, &[j1.into()]);
        self.b.switch_to_block(copied);

        // The realm, the callee's context, and the call. The caller's
        // window is kept as an offset: the register file may move under
        // the callee.
        let realm_at = self.b.ins().load(I64, flags(), self.sh, self.off.s_realm);
        self.b.ins().store(flags(), realm, realm_at, 0);
        let cc = self.b.ins().stack_addr(I64, slot, 0);
        self.b.ins().store(flags(), window, cc, self.off.regs);
        self.b.ins().store(flags(), self.sh, cc, self.off.shared);
        let ic = self.b.ins().load(I64, flags(), s, CALLEE.ic);
        self.b.ins().store(flags(), ic, cc, self.off.ic);
        let info = self.b.ins().load(I64, flags(), s, CALLEE.info);
        self.b.ins().store(flags(), info, cc, self.off.info);
        let pc0 = self.b.ins().iconst(I32, 0);
        self.b.ins().store(flags(), pc0, cc, self.off.pc);
        let none = self.b.ins().iconst(I32, NO_POINT as i64);
        self.b.ins().store(flags(), none, cc, self.off.point);
        self.b.ins().store(flags(), zero, cc, self.off.spill);
        self.b.ins().store(unaligned, undefined, cc, self.off.acc);
        self.store_value(cc, self.off.this, tt, tp);
        let delta = self.b.ins().isub(mine, file);
        let entry = self.b.ins().load(I64, flags(), s, CALLEE.entry);
        let call = self
            .b
            .ins()
            .call_indirect(self.fn_sig, entry, &[cc, self.sh]);
        let r = self.b.inst_results(call)[0];
        let returned = self.b.ins().icmp_imm_u(IntCC::Equal, r, EXIT_RETURN as i64);
        let back = self.b.create_block();
        let left = self.slow_block();
        self.b.ins().brif(returned, back, &[], left, &[]);

        // The callee left compiled code some other way: so does the
        // caller, its frame materialised first. What the callee answered
        // is its own word (a pc of its block, even): the helper answers
        // with the exit code that is to this frame, and only that goes
        // back to the call site.
        self.b.switch_to_block(left);
        self.thunk_tail(self.off.s_left, r);

        // It returned: its frame is the top one. Popping it gives back its
        // reference to the code block, which machine code does only while
        // another holder remains; the last reference is the runtime's to
        // drop.
        self.b.switch_to_block(back);
        let vt = self
            .b
            .ins()
            .load(I64, flags(), cc, self.off.acc + TAG_OFFSET as i32);
        let vp = self
            .b
            .ins()
            .load(I64, flags(), cc, self.off.acc + PAYLOAD_OFFSET as i32);
        let fl = self.b.ins().load(I64, flags(), fv, c.frames.len as i32);
        let frames = self.b.ins().load(I64, flags(), fv, c.frames.ptr as i32);
        let at = self.b.ins().imul_imm_u(fl, size);
        let end = self.b.ins().iadd(frames, at);
        let top = self.b.ins().iadd_imm_s(end, -size);
        let code_w = self.b.ins().load(I64, flags(), top, c.frame_code as i32);
        let count = self.b.ins().load(I64, flags(), code_w, 0);
        let held = self
            .b
            .ins()
            .icmp_imm_u(IntCC::UnsignedGreaterThan, count, 1);
        let pop = self.b.create_block();
        let unpopped = self.slow_block();
        self.b.ins().brif(held, pop, &[], unpopped, &[]);
        self.b.switch_to_block(unpopped);
        self.thunk_returned(pc, vt, vp, false);

        self.b.switch_to_block(pop);
        let count = self.b.ins().iadd_imm_s(count, -1);
        self.b.ins().store(flags(), count, code_w, 0);
        let popped = self.b.ins().iadd_imm_s(fl, -1);
        self.b.ins().store(flags(), popped, fv, c.frames.len as i32);
        let base = self.b.ins().load(I64, flags(), top, c.frame_base as i32);
        self.b
            .ins()
            .store(flags(), base, rv, c.registers.len as i32);
        let caller = self.b.ins().iadd_imm_s(top, -size);
        let realm = self
            .b
            .ins()
            .load(I32, flags(), caller, c.frame_realm as i32);
        self.b.ins().store(flags(), realm, realm_at, 0);
        // The poll after a call returns (J7), and any safe point the callee
        // left wanted.
        let cnt = self.b.ins().load(I32, flags(), self.sh, self.off.s_counter);
        let cnt = self.b.ins().iadd_imm_u(cnt, 1);
        let low = self.b.ins().band_imm_u(cnt, 0xfff);
        let due = self.b.ins().icmp_imm_u(IntCC::Equal, low, 0);
        let attend = self.b.ins().load(I32, flags(), self.sh, self.off.s_attend);
        let wanted = self.b.ins().icmp_imm_u(IntCC::NotEqual, attend, 0);
        let due = self.b.ins().bor(due, wanted);
        let on = self.b.create_block();
        let poll = self.slow_block();
        self.b.ins().brif(due, poll, &[], on, &[]);
        self.b.switch_to_block(poll);
        self.thunk_returned(pc, vt, vp, true);

        // The caller goes on at the next instruction, from its context: the
        // value returned, and its window where the register file is now.
        self.b.switch_to_block(on);
        self.b
            .ins()
            .store(flags(), cnt, self.sh, self.off.s_counter);
        let file = self.b.ins().load(I64, flags(), rv, c.registers.ptr as i32);
        let mine = self.b.ins().iadd(file, delta);
        self.b.ins().store(flags(), mine, self.ctx, self.off.regs);
        self.store_value(self.ctx, self.off.acc, vt, vp);
        let next = self.b.ins().iadd_imm_u(pc, 1);
        self.b.ins().return_(&[next]);

        // Not a call the thunk makes: the runtime's.
        self.b.switch_to_block(miss);
        self.thunk_tail(self.off.s_call, pc);
    }

    // ---- bindings ---------------------------------------------------------------
    //
    // Environment slots and global bindings read in machine code
    // (TASK-101.3.3). Both are loads from the runtime's own storage, found
    // afresh by each instruction: the frame stack and the arenas may move
    // under any call out. Whatever is not as the fast path expects (an
    // environment that is not a declarative record with that slot, a global
    // site of another stamp or realm, a binding that is empty or not a
    // plain value) is the runtime's, through the same entry as before.

    /// The address of the top frame: the one this code is running.
    fn top_frame(&mut self, c: &CallLayout) -> Ir {
        let fv = self.b.ins().load(I64, flags(), self.sh, self.off.s_frames);
        let fl = self.b.ins().load(I64, flags(), fv, c.frames.len as i32);
        let frames = self.b.ins().load(I64, flags(), fv, c.frames.ptr as i32);
        let at = self.b.ins().imul_imm_u(fl, c.frame_size as i64);
        let end = self.b.ins().iadd(frames, at);
        self.b.ins().iadd_imm_s(end, -(c.frame_size as i64))
    }

    /// The address of binding `slot` of the environment `depth` levels out
    /// from the frame's innermost one, or a jump to `miss`.
    fn env_slot(
        &mut self,
        c: &CallLayout,
        e: &EnvLayout,
        depth: u16,
        slot: u16,
        miss: Block,
    ) -> Ir {
        let top = self.top_frame(c);
        let envs = self.b.ins().load(I64, flags(), self.sh, self.off.s_envs);
        // An optional environment id: the frame's, then each parent's.
        let mut option = self.b.ins().iadd_imm_u(top, c.frame_env as i64);
        let mut env = None;
        for _ in 0..=depth {
            let tag = self.b.ins().uload8(I64, flags(), option, e.opt_tag as i32);
            let some = self
                .b
                .ins()
                .icmp_imm_u(IntCC::Equal, tag, e.opt_some as i64);
            self.check(some, miss);
            let id = self.b.ins().uload32(flags(), option, e.opt_id as i32);
            let at = self.b.ins().imul_imm_u(id, e.stride as i64);
            let a = self.b.ins().iadd(envs, at);
            let a = self.b.ins().iadd_imm_u(a, e.offset as i64);
            let kind = self.b.ins().uload8(I64, flags(), a, e.kind as i32);
            let declarative = self
                .b
                .ins()
                .icmp_imm_u(IntCC::Equal, kind, e.declarative as i64);
            self.check(declarative, miss);
            option = self.b.ins().iadd_imm_u(a, e.parent as i64);
            env = Some(a);
        }
        let env = env.expect("at least the innermost environment");
        let len = self.b.ins().load(I64, flags(), env, e.slots_len as i32);
        let has = self
            .b
            .ins()
            .icmp_imm_u(IntCC::UnsignedGreaterThan, len, slot as i64);
        self.check(has, miss);
        let slots = self.b.ins().load(I64, flags(), env, e.slots_ptr as i32);
        self.b.ins().iadd_imm_u(slots, slot as i64 * SIZE as i64)
    }

    /// `LdaEnv` (`acc = slot`) or `StaEnv` (`slot = acc`).
    fn env_access(&mut self, pc: usize, depth: u16, slot: u16, store: bool) {
        // Deep chains stay with the runtime: each level is a few loads.
        let layout = self
            .calls
            .and_then(|c| c.env.map(|e| (c, e)))
            .filter(|_| depth <= 8);
        let Some((c, e)) = layout else {
            self.call_out(self.off.s_fast, pc, None);
            return;
        };
        let miss = self.slow_block();
        let a = self.env_slot(&c, &e, depth, slot, miss);
        if store {
            let (t, p) = self.acc();
            self.store_value(a, 0, t, p);
        } else {
            let t = self.b.ins().load(I64, flags(), a, TAG_OFFSET as i32);
            let p = self.b.ins().load(I64, flags(), a, PAYLOAD_OFFSET as i32);
            self.set_acc(t, p);
        }
        self.next(pc);
        self.b.switch_to_block(miss);
        self.call_out(self.off.s_fast, pc, None);
    }

    /// `LdaGlobal`: the binding's value where its site says it is, while
    /// the site is of the current realm and the runtime's current stamp.
    fn lda_global(&mut self, pc: usize) {
        let k = self.global;
        self.global += 1;
        if !self.calls.is_some_and(|c| c.globals) || self.globals == 0 {
            self.call_out(self.off.s_fast, pc, None);
            return;
        }
        let miss = self.slow_block();
        let at = self.globals + k as usize * std::mem::size_of::<GlobalSite>();
        let s = self.b.ins().iconst(I64, at as i64);
        let stamp_at = self
            .b
            .ins()
            .load(I64, flags(), self.sh, self.off.s_global_stamp);
        let stamp = self.b.ins().load(I64, flags(), stamp_at, 0);
        let filled = self.b.ins().load(I64, flags(), s, GLOBAL.stamp);
        let current = self.b.ins().icmp(IntCC::Equal, stamp, filled);
        let realm_at = self.b.ins().load(I64, flags(), self.sh, self.off.s_realm);
        let realm = self.b.ins().load(I32, flags(), realm_at, 0);
        let site_realm = self.b.ins().load(I32, flags(), s, GLOBAL.realm);
        let same_realm = self.b.ins().icmp(IntCC::Equal, realm, site_realm);
        let ok = self.b.ins().band(current, same_realm);
        self.check(ok, miss);
        let a = self.b.ins().load(I64, flags(), s, GLOBAL.addr);
        let t = self.b.ins().load(I64, flags(), a, TAG_OFFSET as i32);
        let tag = self.b.ins().band_imm_u(t, 0xff);
        // A plain value, and not the empty one (a binding not yet
        // initialised); an accessor's first byte is above every tag.
        let value = self
            .b
            .ins()
            .icmp_imm_u(IntCC::UnsignedLessThan, tag, TAG_EMPTY as i64);
        self.check(value, miss);
        let p = self.b.ins().load(I64, flags(), a, PAYLOAD_OFFSET as i32);
        self.set_acc(t, p);
        self.next(pc);

        // The runtime's cached read, which may fill the site.
        self.b.switch_to_block(miss);
        self.flush_acc();
        let none = Access::Known(Vec::new());
        let next = vec![pc as u32 + 1];
        self.point(pc, PointKind::SafePoint { next }, Some(&none));
        let f = self
            .b
            .ins()
            .load(self.ptr, flags(), self.sh, self.off.s_global);
        let a = self.b.ins().iconst(I32, pc as i64);
        let site = self.b.ins().iconst(I32, k as i64);
        let call = self
            .b
            .ins()
            .call_indirect(self.poll_sig, f, &[self.ctx, a, site]);
        let r = self.b.inst_results(call)[0];
        self.continue_at(r, pc, &[pc + 1]);
    }

    // ---- instructions -------------------------------------------------------

    fn instr(&mut self, pc: usize, instr: Instr) -> Result<(), String> {
        let len = self.code.instrs.len();
        if let Some(off) = branch_offset(&instr) {
            target(len, pc, off)?;
        }
        match instr {
            Instr::LdaUndefined => self.set_acc_const(TAG_UNDEFINED, 0),
            Instr::LdaNull => self.set_acc_const(TAG_NULL, 0),
            Instr::LdaTrue => self.set_acc_const(TAG_BOOL, 1),
            Instr::LdaFalse => self.set_acc_const(TAG_BOOL, 0),
            Instr::LdaEmpty => self.set_acc_const(TAG_EMPTY, 0),
            Instr::LdaInt(i) => self.set_acc_const(TAG_NUMBER, (i as f64).to_bits()),
            Instr::LdaConst(c) => match self.code.constants.get(c as usize) {
                Some(Constant::Number(n)) => self.set_acc_const(TAG_NUMBER, n.to_bits()),
                _ => {
                    self.slow(pc, None);
                    return Ok(());
                }
            },
            Instr::Ldar(r) => {
                let (t, p) = self.load_reg(r);
                self.set_acc(t, p);
            }
            Instr::Star(r) => {
                let v = self.read(Src::Acc);
                self.store_reg(pc, r, v);
            }
            Instr::Mov { dst, src } => {
                let v = self.read_reg(src);
                self.store_reg(pc, dst, v);
            }
            Instr::Nop => {}

            Instr::Add(r) => {
                self.binop(pc, BinOp::Add, Src::Reg(r), Src::Acc);
                return Ok(());
            }
            Instr::Sub(r) => {
                self.binop(pc, BinOp::Sub, Src::Reg(r), Src::Acc);
                return Ok(());
            }
            Instr::Mul(r) => {
                self.binop(pc, BinOp::Mul, Src::Reg(r), Src::Acc);
                return Ok(());
            }
            Instr::Div(r) => {
                self.binop(pc, BinOp::Div, Src::Reg(r), Src::Acc);
                return Ok(());
            }
            Instr::Mod(r) => {
                self.binop(pc, BinOp::Mod, Src::Reg(r), Src::Acc);
                return Ok(());
            }
            Instr::Exp(r) => {
                self.binop(pc, BinOp::Exp, Src::Reg(r), Src::Acc);
                return Ok(());
            }
            Instr::Shl(r) => {
                self.binop(pc, BinOp::Shl, Src::Reg(r), Src::Acc);
                return Ok(());
            }
            Instr::Shr(r) => {
                self.binop(pc, BinOp::Shr, Src::Reg(r), Src::Acc);
                return Ok(());
            }
            Instr::UShr(r) => {
                self.binop(pc, BinOp::UShr, Src::Reg(r), Src::Acc);
                return Ok(());
            }
            Instr::BitAnd(r) => {
                self.binop(pc, BinOp::BitAnd, Src::Reg(r), Src::Acc);
                return Ok(());
            }
            Instr::BitOr(r) => {
                self.binop(pc, BinOp::BitOr, Src::Reg(r), Src::Acc);
                return Ok(());
            }
            Instr::BitXor(r) => {
                self.binop(pc, BinOp::BitXor, Src::Reg(r), Src::Acc);
                return Ok(());
            }
            Instr::Eq(r) => {
                self.binop(pc, BinOp::Eq, Src::Reg(r), Src::Acc);
                return Ok(());
            }
            Instr::Ne(r) => {
                self.binop(pc, BinOp::Ne, Src::Reg(r), Src::Acc);
                return Ok(());
            }
            Instr::StrictEq(r) => {
                self.binop(pc, BinOp::StrictEq, Src::Reg(r), Src::Acc);
                return Ok(());
            }
            Instr::StrictNe(r) => {
                self.binop(pc, BinOp::StrictNe, Src::Reg(r), Src::Acc);
                return Ok(());
            }
            Instr::Lt(r) => {
                self.binop(pc, BinOp::Lt, Src::Reg(r), Src::Acc);
                return Ok(());
            }
            Instr::Le(r) => {
                self.binop(pc, BinOp::Le, Src::Reg(r), Src::Acc);
                return Ok(());
            }
            Instr::Gt(r) => {
                self.binop(pc, BinOp::Gt, Src::Reg(r), Src::Acc);
                return Ok(());
            }
            Instr::Ge(r) => {
                self.binop(pc, BinOp::Ge, Src::Reg(r), Src::Acc);
                return Ok(());
            }
            Instr::BinRR { op, a, b } => {
                self.binop(pc, op, Src::Reg(a), Src::Reg(b));
                return Ok(());
            }
            Instr::BinRI { op, a, imm } => {
                self.binop(pc, op, Src::Reg(a), Src::Imm(imm));
                return Ok(());
            }
            Instr::BinAR { op, b } => {
                self.binop(pc, op, Src::Acc, Src::Reg(b));
                return Ok(());
            }
            Instr::BinAI { op, imm } => {
                self.binop(pc, op, Src::Acc, Src::Imm(imm));
                return Ok(());
            }
            Instr::Inc | Instr::Dec => {
                let v = self.read(Src::Acc);
                let n = self.numbers(&[v]);
                self.fast_or_slow(pc, n, None);
                let x = self.f64(v);
                let one = self.b.ins().f64const(1.0);
                let z = if instr == Instr::Inc {
                    self.b.ins().fadd(x, one)
                } else {
                    self.b.ins().fsub(x, one)
                };
                self.set_acc_number(z);
            }
            Instr::ToNumeric => {
                let v = self.read(Src::Acc);
                let n = self.numbers(&[v]);
                self.fast_or_slow(pc, n, None);
            }
            Instr::CheckTdz(_) => {
                let (t, _) = self.acc();
                let e = self.is_tag(t, TAG_EMPTY);
                let ok = self.b.ins().bxor_imm_u(e, 1);
                self.fast_or_slow(pc, Some(ok), None);
            }

            Instr::Jump(off) => {
                self.edge(pc, (pc as i64 + off as i64) as usize);
                return Ok(());
            }
            Instr::JumpIfTrue(off) => {
                self.truthy_branch(pc, off, true);
                return Ok(());
            }
            Instr::JumpIfFalse(off) => {
                self.truthy_branch(pc, off, false);
                return Ok(());
            }
            Instr::JumpIfNullish(off) => {
                self.tag_branch(pc, off, &[TAG_UNDEFINED, TAG_NULL], true);
                return Ok(());
            }
            Instr::JumpIfNotNullish(off) => {
                self.tag_branch(pc, off, &[TAG_UNDEFINED, TAG_NULL], false);
                return Ok(());
            }
            Instr::JumpIfUndefined(off) => {
                self.tag_branch(pc, off, &[TAG_UNDEFINED], true);
                return Ok(());
            }
            Instr::JumpIfNotUndefined(off) => {
                self.tag_branch(pc, off, &[TAG_UNDEFINED], false);
                return Ok(());
            }
            Instr::JumpIfEmpty(off) => {
                self.tag_branch(pc, off, &[TAG_EMPTY], true);
                return Ok(());
            }
            Instr::JumpIfNotEmpty(off) => {
                self.tag_branch(pc, off, &[TAG_EMPTY], false);
                return Ok(());
            }
            Instr::JumpIfObject(off) => {
                self.tag_branch(pc, off, &[TAG_OBJECT], true);
                return Ok(());
            }
            Instr::JumpIfStrictEq { reg, offset } => {
                self.compare_branch(pc, BinOp::StrictEq, Src::Reg(reg), Src::Acc, offset, true);
                return Ok(());
            }
            Instr::JumpIfCmpRR { op, a, b, offset } => {
                self.compare_branch(pc, op, Src::Reg(a), Src::Reg(b), offset, true);
                return Ok(());
            }
            Instr::JumpIfNotCmpRR { op, a, b, offset } => {
                self.compare_branch(pc, op, Src::Reg(a), Src::Reg(b), offset, false);
                return Ok(());
            }
            Instr::JumpIfCmpRI { op, a, imm, offset } => {
                self.compare_branch(pc, op, Src::Reg(a), Src::Imm(imm), offset, true);
                return Ok(());
            }
            Instr::JumpIfNotCmpRI { op, a, imm, offset } => {
                self.compare_branch(pc, op, Src::Reg(a), Src::Imm(imm), offset, false);
                return Ok(());
            }
            Instr::JumpIfCmpRA { op, a, offset } => {
                self.compare_branch(pc, op, Src::Reg(a), Src::Acc, offset, true);
                return Ok(());
            }
            Instr::JumpIfNotCmpRA { op, a, offset } => {
                self.compare_branch(pc, op, Src::Reg(a), Src::Acc, offset, false);
                return Ok(());
            }
            Instr::JumpIfCmpAI { op, imm, offset } => {
                self.compare_branch(pc, op, Src::Acc, Src::Imm(imm), offset, true);
                return Ok(());
            }
            Instr::JumpIfNotCmpAI { op, imm, offset } => {
                self.compare_branch(pc, op, Src::Acc, Src::Imm(imm), offset, false);
                return Ok(());
            }

            Instr::GetNamed { obj, ic, .. } => {
                self.get_named(pc, obj, ic);
                return Ok(());
            }
            Instr::SetNamed { obj, ic, .. } => {
                self.set_named(pc, obj, ic);
                return Ok(());
            }
            Instr::GetKeyed { obj, key } => {
                self.get_keyed(pc, obj, key);
                return Ok(());
            }
            Instr::SetKeyed { obj, key, .. } => {
                self.set_keyed(pc, obj, key);
                return Ok(());
            }

            Instr::Call {
                callee,
                this,
                args,
                argc,
            } => {
                self.call(pc, callee, this, args, argc);
                return Ok(());
            }
            // The frame's `this`, fixed for the run, is in the context.
            Instr::LdaThis => {
                let o = self.off.this;
                let t = self.b.ins().load(I64, flags(), self.ctx, o + TAG_OFFSET as i32);
                let p = self.b.ins().load(I64, flags(), self.ctx, o + PAYLOAD_OFFSET as i32);
                self.set_acc(t, p);
            }
            Instr::LdaGlobal(_) => {
                self.lda_global(pc);
                return Ok(());
            }
            Instr::LdaEnv { depth, slot } => {
                self.env_access(pc, depth, slot, false);
                return Ok(());
            }
            Instr::StaEnv { depth, slot } => {
                self.env_access(pc, depth, slot, true);
                return Ok(());
            }
            Instr::LdaCallee => {
                self.call_out(self.off.s_fast, pc, None);
                return Ok(());
            }

            Instr::Return => {
                self.flush_acc();
                let next = self.b.ins().iconst(I32, pc as i64 + 1);
                self.b.ins().store(flags(), next, self.ctx, self.off.pc);
                let v = self.b.ins().iconst(I32, EXIT_RETURN as i64);
                self.b.ins().return_(&[v]);
                return Ok(());
            }

            other => {
                let to = branch_offset(&other).map(|off| (pc as i64 + off as i64) as usize);
                self.slow(pc, to);
                return Ok(());
            }
        }
        self.next(pc);
        Ok(())
    }
}

/// For an equality operator, whether it is the negated one (`!=`,
/// `!==`).
fn equality(op: BinOp) -> Option<bool> {
    match op {
        BinOp::Eq | BinOp::StrictEq => Some(false),
        BinOp::Ne | BinOp::StrictNe => Some(true),
        _ => None,
    }
}

fn loose(op: BinOp) -> bool {
    matches!(op, BinOp::Eq | BinOp::Ne)
}

fn float_cc(op: BinOp) -> FloatCC {
    match op {
        BinOp::Eq | BinOp::StrictEq => FloatCC::Equal,
        BinOp::Ne | BinOp::StrictNe => FloatCC::NotEqual,
        BinOp::Lt => FloatCC::LessThan,
        BinOp::Le => FloatCC::LessThanOrEqual,
        BinOp::Gt => FloatCC::GreaterThan,
        _ => FloatCC::GreaterThanOrEqual,
    }
}
