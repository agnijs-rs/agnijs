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

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use agnijs_js_compiler::bytecode::{
    BinOp, CodeBlock, Constant, IcEntry, IcSite, IcState, Instr, IC_ENTRIES,
};
use cranelift_codegen::control::ControlPlane;
use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::types::{F64, I32, I64, I64X2};
use cranelift_codegen::ir::{
    AbiParam, Block, Function, InstBuilder, MemFlagsData, SigRef, Signature, StackSlot,
    StackSlotData, StackSlotKind, Type, UserFuncName, Value as Ir,
};
use cranelift_codegen::isa::OwnedTargetIsa;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Switch, Variable};

use crate::exec::{Offsets, EXIT_INTERP, EXIT_PANIC, EXIT_RETURN};
use crate::handoff::{Location, Point, PointKind, Points};
use crate::layout::*;
use crate::{Arith, HeapLayout, Speculation};

/// What [`compile`] produces.
pub(crate) struct Lowered {
    pub bytes: Vec<u8>,
    /// The pcs compiled code can be entered at, ascending.
    pub entries: Vec<u32>,
    /// The safe points, side exits and entry guards of the code.
    pub points: Points,
}

/// How many registers a block keeps in machine state at most.
const MAX_CACHED: usize = 8;

/// The registers an instruction's runtime path reads: a known few, or
/// (a generic instruction, whose semantics the code generator does not
/// decode) possibly any, and then possibly written too.
enum Access {
    Known(Vec<u16>),
    Any,
}

fn access(instr: &Instr) -> Access {
    use Instr::*;
    Access::Known(match *instr {
        Ldar(r) | Add(r) | Sub(r) | Mul(r) | Div(r) | Mod(r) | Exp(r) | Shl(r) | Shr(r)
        | UShr(r) | BitAnd(r) | BitOr(r) | BitXor(r) | Eq(r) | Ne(r) | StrictEq(r)
        | StrictNe(r) | Lt(r) | Le(r) | Gt(r) | Ge(r) => vec![r],
        Mov { src, .. } => vec![src],
        BinRR { a, b, .. } | JumpIfCmpRR { a, b, .. } | JumpIfNotCmpRR { a, b, .. } => vec![a, b],
        BinRI { a, .. }
        | JumpIfCmpRI { a, .. }
        | JumpIfNotCmpRI { a, .. }
        | JumpIfCmpRA { a, .. }
        | JumpIfNotCmpRA { a, .. } => vec![a],
        BinAR { b, .. } => vec![b],
        JumpIfStrictEq { reg, .. } => vec![reg],
        GetNamed { obj, .. } | SetNamed { obj, .. } => vec![obj],
        GetKeyed { obj, key } | SetKeyed { obj, key, .. } => vec![obj, key],
        Call {
            callee,
            this,
            args,
            argc,
        } => {
            let mut v = vec![callee, this];
            v.extend((0..argc).map(|i| args + i));
            v
        }
        Star(_) | BinAI { .. } | JumpIfCmpAI { .. } | JumpIfNotCmpAI { .. } | Inc | Dec
        | ToNumeric | CheckTdz(_) | LdaUndefined | LdaNull | LdaTrue | LdaFalse | LdaEmpty
        | LdaInt(_) | LdaConst(_) | LdaThis | Nop | Return | Jump(_) | JumpIfTrue(_)
        | JumpIfFalse(_) | JumpIfNullish(_) | JumpIfNotNullish(_) | JumpIfUndefined(_)
        | JumpIfNotUndefined(_) | JumpIfEmpty(_) | JumpIfNotEmpty(_) | JumpIfObject(_)
        | LdaGlobal(_) | LdaEnv { .. } | StaEnv { .. } | LdaCallee => vec![],
        _ => return Access::Any,
    })
}

/// The register an instruction's inline expansion writes.
fn written(instr: &Instr) -> Option<u16> {
    match *instr {
        Instr::Star(r) | Instr::Mov { dst: r, .. } => Some(r),
        _ => None,
    }
}

/// How deep in loops each instruction is (a back-edge's range is a loop).
fn loop_depth(code: &CodeBlock) -> Vec<u8> {
    let len = code.instrs.len();
    let mut depth = vec![0u8; len];
    for (pc, instr) in code.instrs.iter().enumerate() {
        if let Some(off) = branch_offset(instr) {
            if off <= 0 {
                let t = (pc as i64 + off as i64).max(0) as usize;
                for d in &mut depth[t..=pc] {
                    *d = d.saturating_add(1);
                }
            }
        }
    }
    depth
}

/// The registers to keep in machine state: those loops read or write by
/// inline-expanded instructions, the most used first, the speculated ones
/// before the rest. A block without loops caches nothing: its call outs
/// would pay for the caching and nothing would gain.
fn choose_cache(code: &CodeBlock, spec: &Speculation) -> Vec<(u16, bool)> {
    let depth = loop_depth(code);
    let mut score: BTreeMap<u16, u64> = BTreeMap::new();
    for (pc, instr) in code.instrs.iter().enumerate() {
        let w = match depth[pc] {
            0 => continue,
            1 => 8,
            _ => 64,
        };
        let mut refs = match access(instr) {
            Access::Known(r) => r,
            Access::Any => Vec::new(),
        };
        refs.extend(written(instr));
        if matches!(instr, Instr::Call { .. }) {
            // Read by the runtime, not inline: caching them helps nothing.
            refs.clear();
        }
        for r in refs {
            *score.entry(r).or_default() += w;
        }
    }
    let speculated = |r: u16| r < 64 && spec.numbers & (1 << r) != 0;
    let mut regs: Vec<(u16, u64)> = score.into_iter().collect();
    regs.sort_by(|a, b| {
        speculated(b.0)
            .cmp(&speculated(a.0))
            .then(b.1.cmp(&a.1))
            .then(a.0.cmp(&b.0))
    });
    regs.truncate(MAX_CACHED);
    regs.into_iter().map(|(r, _)| (r, speculated(r))).collect()
}

/// The pcs an instruction may continue at.
fn successors(code: &CodeBlock, pc: usize) -> Vec<usize> {
    let instr = &code.instrs[pc];
    match *instr {
        Instr::Return => vec![],
        Instr::Jump(off) => vec![(pc as i64 + off as i64) as usize],
        _ => {
            let mut s = vec![pc + 1];
            if let Some(off) = branch_offset(instr) {
                s.push((pc as i64 + off as i64) as usize);
            }
            s
        }
    }
}

/// For each pc, the cached registers (a bit per cache index) that may be
/// read before written from there: what must hold its value when compiled
/// code is entered or resumed at that pc. A generic instruction may read
/// any register and is assumed to write none.
fn liveness(code: &CodeBlock, index: &[Option<u8>], n: usize) -> Vec<u16> {
    let len = code.instrs.len();
    let all: u16 = if n >= 16 { u16::MAX } else { (1u16 << n) - 1 };
    let bit = |r: u16| -> u16 {
        index
            .get(r as usize)
            .copied()
            .flatten()
            .map_or(0, |i| 1 << i)
    };
    let mut live_in = vec![0u16; len + 1];
    loop {
        let mut changed = false;
        for pc in (0..len).rev() {
            let instr = &code.instrs[pc];
            let mut out = 0;
            for s in successors(code, pc) {
                out |= live_in.get(s).copied().unwrap_or(0);
            }
            let new = match access(instr) {
                Access::Any => all,
                Access::Known(reads) => {
                    let uses = reads.iter().fold(0, |m, &r| m | bit(r));
                    let def = written(instr).map_or(0, bit);
                    uses | (out & !def)
                }
            };
            if new != live_in[pc] {
                live_in[pc] = new;
                changed = true;
            }
        }
        if !changed {
            return live_in;
        }
    }
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

/// The branch offset of an instruction that may branch, for the
/// instructions whose taken edge can come back from the runtime.
pub(crate) fn branch_offset(instr: &Instr) -> Option<i32> {
    Some(match *instr {
        Instr::Jump(o)
        | Instr::JumpIfTrue(o)
        | Instr::JumpIfFalse(o)
        | Instr::JumpIfNullish(o)
        | Instr::JumpIfNotNullish(o)
        | Instr::JumpIfUndefined(o)
        | Instr::JumpIfNotUndefined(o)
        | Instr::JumpIfEmpty(o)
        | Instr::JumpIfNotEmpty(o)
        | Instr::JumpIfObject(o) => o,
        Instr::JumpIfStrictEq { offset, .. }
        | Instr::JumpIfCmpRR { offset, .. }
        | Instr::JumpIfNotCmpRR { offset, .. }
        | Instr::JumpIfCmpRI { offset, .. }
        | Instr::JumpIfNotCmpRI { offset, .. }
        | Instr::JumpIfCmpRA { offset, .. }
        | Instr::JumpIfNotCmpRA { offset, .. }
        | Instr::JumpIfCmpAI { offset, .. }
        | Instr::JumpIfNotCmpAI { offset, .. } => offset,
        Instr::IteratorStep { done, .. }
        | Instr::IteratorResult { done }
        | Instr::ForInNext { done, .. } => done,
        _ => return None,
    })
}

/// Whether the instruction never calls out of compiled code.
fn always_inline(code: &CodeBlock, instr: &Instr) -> bool {
    match *instr {
        Instr::LdaConst(c) => matches!(code.constants.get(c as usize), Some(Constant::Number(_))),
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
        | Instr::LdaThis
        | Instr::Jump(_)
        | Instr::JumpIfNullish(_)
        | Instr::JumpIfNotNullish(_)
        | Instr::JumpIfUndefined(_)
        | Instr::JumpIfNotUndefined(_)
        | Instr::JumpIfEmpty(_)
        | Instr::JumpIfNotEmpty(_)
        | Instr::JumpIfObject(_)
        | Instr::Return => true,
        _ => false,
    }
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
/// inline-cache hit paths when `heap` describes the runtime's objects.
pub(crate) fn compile(
    code: &CodeBlock,
    off: &Offsets,
    heap: Option<&HeapLayout>,
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
    sig.returns.push(AbiParam::new(I32));
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
        let regs = b.declare_var(ptr);
        let tag = b.declare_var(I64);
        let pay = b.declare_var(I64);
        let cached: Vec<Cached> = chosen
            .iter()
            .map(|&(reg, number)| Cached {
                reg,
                repr: if number {
                    Repr::Number(b.declare_var(I64))
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
        let mut lower = Lower {
            b,
            code,
            off,
            ptr,
            ctx,
            regs,
            tag,
            pay,
            cached,
            index,
            live_in,
            spill,
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
    })
}

/// An operand of a binary operator.
#[derive(Clone, Copy)]
enum Src {
    Acc,
    Reg(u16),
    Imm(i32),
}

/// A value in two machine words; `tag` is `None` for an immediate number.
#[derive(Clone, Copy)]
struct Val {
    tag: Option<Ir>,
    pay: Ir,
}

/// How a cached register is held.
#[derive(Clone, Copy)]
enum Repr {
    /// Tag word and payload word, as in the register file.
    Boxed { tag: Variable, pay: Variable },
    /// The bits of an `f64`: the register was speculated to hold a number.
    Number(Variable),
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
    ctx: Ir,
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
    /// before read, so its payload is taken as it is (a non-number's shows
    /// as a number only in a debugger's view of a variable not yet
    /// assigned).
    fn load_cached(&mut self, pc: usize, kind: PointKind) {
        for i in 0..self.cached.len() {
            let c = self.cached[i];
            let (t, p) = self.load_file(c.reg);
            match c.repr {
                Repr::Boxed { tag, pay } => {
                    self.b.def_var(tag, t);
                    self.b.def_var(pay, p);
                }
                Repr::Number(bits) => {
                    if self.live_at(pc, i) {
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
    fn point(&mut self, pc: usize, kind: PointKind, dirty: Option<&Access>) -> u32 {
        let mut regs = Vec::new();
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
            let (base, off) = self.spill_addr(i);
            match c.repr {
                Repr::Boxed { tag, pay } => {
                    let t = self.b.use_var(tag);
                    let p = self.b.use_var(pay);
                    self.store_value(base, off, t, p);
                    regs.push((c.reg, Location::Spill(off)));
                }
                Repr::Number(bits) => {
                    let v = self.b.use_var(bits);
                    self.b.ins().store(flags(), v, base, off);
                    regs.push((c.reg, Location::SpillNumber(off)));
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
            Repr::Number(bits) => {
                let t = self.b.ins().iconst(I64, TAG_NUMBER as i64);
                (t, self.b.use_var(bits))
            }
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
        let f = self.b.ins().load(self.ptr, flags(), self.ctx, self.off.deopt);
        let a = self.b.ins().iconst(I32, id as i64);
        let call = self.b.ins().call_indirect(self.slow_sig, f, &[self.ctx, a]);
        let r = self.b.inst_results(call)[0];
        self.b.ins().return_(&[r]);
    }

    /// Reload after the runtime ran the instruction at `pc` and continues
    /// at `to`: a generic instruction may have written any register, so
    /// every cached one is reloaded (speculated ones guarded, a failure
    /// leaving at `to` with nothing dirty); a known one wrote none.
    fn reload_cached(&mut self, pc: usize, to: usize) {
        if let Access::Any = access(&self.code.instrs[pc]) {
            self.load_cached(to, PointKind::Exit { guard: None });
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

    fn load_reg(&mut self, r: u16) -> (Ir, Ir) {
        let v = self.read_reg(r);
        let t = match v.tag {
            Some(t) => t,
            None => self.b.ins().iconst(I64, TAG_NUMBER as i64),
        };
        (t, v.pay)
    }

    /// Register `r` as a [`Val`]: without a tag when it is a speculated
    /// number, which its readers then need not test.
    fn read_reg(&mut self, r: u16) -> Val {
        match self.cached(r).map(|c| c.repr) {
            Some(Repr::Boxed { tag, pay }) => Val {
                tag: Some(self.b.use_var(tag)),
                pay: self.b.use_var(pay),
            },
            Some(Repr::Number(bits)) => Val {
                tag: None,
                pay: self.b.use_var(bits),
            },
            None => {
                let (t, p) = self.load_file(r);
                Val { tag: Some(t), pay: p }
            }
        }
    }

    /// `r = v` by the instruction at `pc`: into a speculated register only
    /// a number goes; anything else is the guard failure that leaves at
    /// `pc`, before the instruction.
    fn store_reg(&mut self, pc: usize, r: u16, v: Val) {
        match self.cached(r).map(|c| c.repr) {
            Some(Repr::Boxed { tag, pay }) => {
                let t = match v.tag {
                    Some(t) => t,
                    None => self.b.ins().iconst(I64, TAG_NUMBER as i64),
                };
                self.b.def_var(tag, t);
                self.b.def_var(pay, v.pay);
            }
            Some(Repr::Number(bits)) => {
                if let Some(t) = v.tag {
                    let ok = self.is_tag(t, TAG_NUMBER);
                    let (yes, no) = self.guard(ok);
                    self.b.set_cold_block(no);
                    self.b.switch_to_block(no);
                    self.leave_at(pc, PointKind::Exit { guard: Some(r) }, true);
                    self.b.switch_to_block(yes);
                }
                self.b.def_var(bits, v.pay);
            }
            None => {
                let t = match v.tag {
                    Some(t) => t,
                    None => self.b.ins().iconst(I64, TAG_NUMBER as i64),
                };
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
                Val { tag: Some(t), pay: p }
            }
            Src::Reg(r) => self.read_reg(r),
            Src::Imm(i) => Val {
                tag: None,
                pay: self.b.ins().iconst(I64, (i as f64).to_bits() as i64),
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
        let c = self.b.ins().load(I32, flags(), self.ctx, self.off.counter);
        let c = self.b.ins().iadd_imm_u(c, 1);
        self.b.ins().store(flags(), c, self.ctx, self.off.counter);
        let low = self.b.ins().band_imm_u(c, 0xfff);
        let due = self.b.ins().icmp_imm_u(IntCC::Equal, low, 0);
        // Or a safe point is wanted at every back-edge (GC stress).
        let attend = self.b.ins().load(I32, flags(), self.ctx, self.off.attend);
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
        self.point(pc, PointKind::SafePoint { next: vec![to as u32] }, Some(&none));
        let f = self.b.ins().load(self.ptr, flags(), self.ctx, self.off.poll);
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
        self.call_out(self.off.slow, pc, to);
    }

    /// [`Lower::slow`] through the runtime entry at `entry` in the context.
    fn call_out(&mut self, entry: i32, pc: usize, to: Option<usize>) {
        self.flush_acc();
        let mut succ = vec![pc + 1];
        succ.extend(to);
        let reads = access(&self.code.instrs[pc]);
        let next = succ.iter().map(|&s| s as u32).collect();
        self.point(pc, PointKind::SafePoint { next }, Some(&reads));
        let f = self.b.ins().load(self.ptr, flags(), self.ctx, entry);
        let a = self.b.ins().iconst(I32, pc as i64);
        let call = self.b.ins().call_indirect(self.slow_sig, f, &[self.ctx, a]);
        let r = self.b.inst_results(call)[0];
        self.continue_at(r, pc, &succ);
    }

    /// After a call out from the instruction at `pc` returned `r`: reload
    /// the context and whatever the runtime may have written, and continue
    /// at whichever successor `r` names; else return `r` (an exit code: the
    /// runtime materialised the frame for any pc it had no successor for).
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
                let a = if op == BinOp::Mod { Arith::Mod } else { Arith::Exp };
                let f = self.b.ins().load(self.ptr, flags(), self.ctx, self.off.arith);
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
        let both = self.numbers(&[lv, rv]);
        self.fast_or_slow(pc, both, Some(to));
        let x = self.f64(lv);
        let y = self.f64(rv);
        let c = self.b.ins().fcmp(float_cc(op), x, y);
        let c = if when { c } else { self.b.ins().bxor_imm_u(c, 1) };
        self.branch(pc, c, to);
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
        let base = self.b.ins().load(self.ptr, flags(), self.ctx, self.off.objects);
        let id = if self.ptr == I64 { id } else { self.b.ins().ireduce(self.ptr, id) };
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
            let epoch = self.b.ins().uload32(flags(), self.ctx, self.off.proto_epoch);
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

            Instr::Call { .. } => {
                self.call_out(self.off.call, pc, None);
                return Ok(());
            }
            // The frame's `this`, fixed for the run, is in the context.
            Instr::LdaThis => {
                let o = self.off.this;
                let t = self.b.ins().load(I64, flags(), self.ctx, o + TAG_OFFSET as i32);
                let p = self.b.ins().load(I64, flags(), self.ctx, o + PAYLOAD_OFFSET as i32);
                self.set_acc(t, p);
            }
            Instr::LdaGlobal(_) | Instr::LdaEnv { .. } | Instr::StaEnv { .. } | Instr::LdaCallee => {
                self.call_out(self.off.fast, pc, None);
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
