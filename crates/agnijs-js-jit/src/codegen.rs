//! Bytecode to Cranelift IR (ADR 0020). Safe code: it produces bytes,
//! which [`crate::exec`] maps and runs.
//!
//! One machine function per code block, `extern "C" fn(*mut Ctx) -> u32`.
//! It starts with a switch on the entry pc (the function's start, loop
//! headers, and every instruction after one that may call out) to a resume
//! block that loads the register window and the accumulator from the
//! context. Each instruction is a block; the accumulator lives in two
//! 64-bit variables (tag word, payload word) between instructions and is
//! flushed to the context before any call out. Registers are read and
//! written in the register file itself, never cached.
//!
//! Fast paths are guarded by tag tests and never speculate: a failed guard
//! calls the runtime's `slow` entry, which runs the instruction's generic
//! semantics and returns the pc to continue at, or an exit code.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use agnijs_js_compiler::bytecode::{BinOp, CodeBlock, Constant, Instr};
use cranelift_codegen::control::ControlPlane;
use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::types::{F64, I32, I64};
use cranelift_codegen::ir::{
    AbiParam, Block, Function, InstBuilder, MemFlagsData, SigRef, Signature, Type, UserFuncName,
    Value as Ir,
};
use cranelift_codegen::isa::OwnedTargetIsa;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Switch, Variable};

use crate::exec::{Offsets, EXIT_INTERP, EXIT_PANIC, EXIT_RETURN};
use crate::layout::*;
use crate::Arith;

/// What [`compile`] produces.
pub(crate) struct Lowered {
    pub bytes: Vec<u8>,
    /// The pcs compiled code can be entered at, ascending.
    pub entries: Vec<u32>,
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
fn branch_offset(instr: &Instr) -> Option<i32> {
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

/// Compile `code` against a context with field offsets `off`.
pub(crate) fn compile(code: &CodeBlock, off: &Offsets) -> Result<Lowered, String> {
    let isa = isa()?;
    let ptr = isa.pointer_type();
    let cc = isa.default_call_conv();
    let entries = entries(code)?;

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
    {
        let mut b = FunctionBuilder::new(&mut func, &mut fctx);
        let slow_sig = b.import_signature(slow_sig);
        let poll_sig = b.import_signature(poll_sig);
        let arith_sig = b.import_signature(arith_sig);
        let regs = b.declare_var(ptr);
        let tag = b.declare_var(I64);
        let pay = b.declare_var(I64);

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
            blocks,
            resume,
            slow_sig,
            poll_sig,
            arith_sig,
        };
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
    }
    let bytes = emit(func)?;
    Ok(Lowered {
        bytes,
        entries: entries.into_iter().collect(),
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

struct Lower<'a, 'c> {
    b: FunctionBuilder<'a>,
    code: &'c CodeBlock,
    off: &'c Offsets,
    ptr: Type,
    ctx: Ir,
    regs: Variable,
    tag: Variable,
    pay: Variable,
    blocks: Vec<Block>,
    resume: Vec<Option<Block>>,
    slow_sig: SigRef,
    poll_sig: SigRef,
    arith_sig: SigRef,
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
        self.b.ins().store(flags(), t, self.ctx, self.off.acc);
        self.b.ins().store(flags(), p, self.ctx, self.off.acc + 8);
    }

    // ---- values -----------------------------------------------------------

    fn load_reg(&mut self, r: u16) -> (Ir, Ir) {
        let regs = self.b.use_var(self.regs);
        let o = r as i32 * SIZE as i32;
        let t = self.b.ins().load(I64, flags(), regs, o + TAG_OFFSET as i32);
        let p = self.b.ins().load(I64, flags(), regs, o + PAYLOAD_OFFSET as i32);
        (t, p)
    }

    fn store_reg(&mut self, r: u16, t: Ir, p: Ir) {
        let regs = self.b.use_var(self.regs);
        let o = r as i32 * SIZE as i32;
        self.b.ins().store(flags(), t, regs, o + TAG_OFFSET as i32);
        self.b.ins().store(flags(), p, regs, o + PAYLOAD_OFFSET as i32);
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
            Src::Reg(r) => {
                let (t, p) = self.load_reg(r);
                Val { tag: Some(t), pay: p }
            }
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
        let poll = self.b.create_block();
        self.b.set_cold_block(poll);
        let blk = self.blocks[to];
        self.b.ins().brif(due, poll, &[], blk, &[]);
        self.b.switch_to_block(poll);
        self.flush_acc();
        let f = self.b.ins().load(self.ptr, flags(), self.ctx, self.off.poll);
        let a = self.b.ins().iconst(I32, pc as i64);
        let t = self.b.ins().iconst(I32, to as i64);
        let call = self.b.ins().call_indirect(self.poll_sig, f, &[self.ctx, a, t]);
        let r = self.b.inst_results(call)[0];
        self.continue_at(r, &[to]);
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
        self.flush_acc();
        let f = self.b.ins().load(self.ptr, flags(), self.ctx, self.off.slow);
        let a = self.b.ins().iconst(I32, pc as i64);
        let call = self.b.ins().call_indirect(self.slow_sig, f, &[self.ctx, a]);
        let r = self.b.inst_results(call)[0];
        let mut succ = vec![pc + 1];
        succ.extend(to);
        self.continue_at(r, &succ);
    }

    /// After a call out returned `r`: the resume block of whichever
    /// successor it names, else return `r` (an exit code, or a pc compiled
    /// code has no successor for, where the interpreter continues).
    fn continue_at(&mut self, r: Ir, succ: &[usize]) {
        for &s in succ {
            let Some(Some(blk)) = self.resume.get(s).copied() else {
                continue;
            };
            let other = self.b.create_block();
            let hit = self.b.ins().icmp_imm_u(IntCC::Equal, r, s as i64);
            self.b.ins().brif(hit, blk, &[], other, &[]);
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
                let (t, p) = self.acc();
                self.store_reg(r, t, p);
            }
            Instr::Mov { dst, src } => {
                let (t, p) = self.load_reg(src);
                self.store_reg(dst, t, p);
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
