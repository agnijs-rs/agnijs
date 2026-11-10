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
//! Compiled code never holds a JavaScript value anywhere the collector
//! cannot see across a call out of it: the accumulator is flushed to the
//! frame and the register file stays in memory, so collections under
//! compiled code find their roots in the frame stack as they do under the
//! interpreter, and nothing needs a GC map yet. It never speculates: every
//! inline fast path is guarded, and a failed guard runs the instruction's
//! generic semantics instead, so nothing needs deoptimization yet either.
//! Control leaves compiled code only at instruction boundaries where the
//! frame is already the interpreter's ([`Exit`]). Both reserved pieces,
//! deoptimization and GC maps, are specified in [`handoff`].
//!
//! Everything here is safe code except [`exec`]-memory handling, the entry
//! trampoline and the helper trampolines, which live in one small module
//! (ADR 0019 J10).

#![deny(unsafe_code)]

use std::marker::PhantomData;
use std::time::Duration;

use agnijs_js_compiler::bytecode::{CodeBlock, CodeKind, Instr};
use agnijs_js_parser::ast::FunctionKind;

#[cfg(jit_host)]
mod codegen;
mod exec;
pub mod handoff;

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
    /// A loop back-edge from the jump at `pc` to `target` whose interrupt
    /// poll is due (the counter, already advanced, is a multiple of
    /// 0x1000): ask the host. `Next` continues at `target`.
    fn poll(&mut self, pc: u32, target: u32, acc: V) -> Resume<V>;
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
/// and class constructors bind `this` late; eval and module code run once;
/// direct eval and `super()` inspect the frame in ways compiled code does
/// not model; `debugger` belongs to the interpreter (ADR 0019 J9).
pub fn eligible(code: &CodeBlock) -> Result<(), Ineligible> {
    if code.function_kind != FunctionKind::Normal
        || !matches!(
            code.kind,
            CodeKind::Script | CodeKind::Function | CodeKind::Arrow | CodeKind::Method
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
    let mut cost = 0usize;
    for instr in code.instrs.iter() {
        cost += match instr {
            i if expands_inline(code, i) => 0,
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

/// One code block's machine code.
pub struct Compiled<V> {
    #[cfg_attr(not(jit_host), allow(dead_code))]
    mem: exec::ExecMemory,
    /// `entry[pc]`: compiled code can start at `pc`.
    entry: Box<[bool]>,
    registers: usize,
    /// Size of the machine code in bytes.
    pub code_size: usize,
    /// Time spent generating it.
    pub compile_time: Duration,
    _value: PhantomData<fn(V) -> V>,
}

impl<V: JitValue> Compiled<V> {
    /// Compile `code`, which must be [`eligible`].
    pub fn compile(code: &CodeBlock) -> Result<Self, Ineligible> {
        eligible(code)?;
        if !available() {
            return Err(Ineligible::Unavailable);
        }
        if !exec::layout_matches::<V>() {
            return Err(Ineligible::Layout);
        }
        Self::generate(code)
    }

    #[cfg(jit_host)]
    fn generate(code: &CodeBlock) -> Result<Self, Ineligible> {
        let start = std::time::Instant::now();
        let lowered = codegen::compile(code, &exec::offsets::<V>()).map_err(Ineligible::Compile)?;
        let mem = exec::ExecMemory::new(&lowered.bytes).ok_or(Ineligible::Unavailable)?;
        let mut entry = vec![false; code.instrs.len()].into_boxed_slice();
        for &pc in &lowered.entries {
            entry[pc as usize] = true;
        }
        Ok(Compiled {
            mem,
            entry,
            registers: code.register_count as usize,
            code_size: lowered.bytes.len(),
            compile_time: start.elapsed(),
            _value: PhantomData,
        })
    }

    #[cfg(not(jit_host))]
    fn generate(_code: &CodeBlock) -> Result<Self, Ineligible> {
        Err(Ineligible::Unavailable)
    }

    /// Whether compiled code can start at `pc`: the function's start, a
    /// loop header, or the instruction after one that may call out.
    #[inline]
    pub fn is_entry(&self, pc: usize) -> bool {
        self.entry.get(pc).copied().unwrap_or(false)
    }

    /// Run from `pc` (an entry, else [`Exit::Interp`] at once) with the
    /// accumulator `acc`, the top frame being this function's.
    pub fn enter<R: Runtime<V>>(&self, rt: &mut R, pc: u32, acc: V) -> Outcome<V> {
        if !self.is_entry(pc as usize) {
            return Outcome {
                exit: Exit::Interp,
                pc,
                acc,
            };
        }
        exec::enter(&self.mem, self.registers, rt, pc, acc)
    }
}
