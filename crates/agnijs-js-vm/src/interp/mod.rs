//! The bytecode interpreter.
//!
//! JavaScript-to-JavaScript calls push a [`Frame`] and continue in the same
//! loop; only native functions, generator resumption and async bodies
//! recurse on the Rust stack. Registers of every frame live in one shared
//! register file (`Vm::regs`), so the collector finds them in one place.

pub mod calls;
pub mod generator;
pub mod globals;
pub mod ic;
pub mod step;

use crate::convert::{number_arith, number_bitwise};
use crate::value::{EnvId, ObjId, Value};
use crate::vm::{Fault, JsResult, Vm};
use agnijs_engine_api::RealmId;
use agnijs_js_compiler::bytecode::{CodeBlock, Constant, Instr};
use std::rc::Rc;
use step::instr_arith;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConstructKind {
    /// A plain call.
    None,
    /// `new` on a base constructor: `this` is the fresh object.
    Base,
    /// `new` on a derived constructor: `this` starts uninitialised.
    Derived,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameKind {
    Function,
    Script,
    Eval,
    Module,
}

pub struct Frame {
    pub code: Rc<CodeBlock>,
    pub pc: usize,
    /// Index of register 0 in `Vm::regs`.
    pub base: usize,
    pub acc: Value,
    /// The innermost environment.
    pub env: Option<EnvId>,
    /// The environment at entry (depth 0), for handler unwinding.
    pub base_env: Option<EnvId>,
    pub env_depth: u16,
    pub this: Value,
    pub callee: Option<ObjId>,
    pub new_target: Value,
    /// The actual arguments, kept only when the code needs them.
    pub args: Vec<Value>,
    pub argc: usize,
    pub realm: RealmId,
    /// Return to the Rust caller of `run` when this frame finishes.
    pub exit: bool,
    pub construct: ConstructKind,
    pub kind: FrameKind,
    /// The generator or async-function state object driving this frame.
    pub generator: Option<ObjId>,
    pub module: Option<ObjId>,
}

/// How a run of the interpreter ended.
#[derive(Clone, Copy, Debug)]
pub enum RunOutcome {
    /// The exit frame returned.
    Return(Value),
    /// A generator frame yielded (the frame has been saved).
    Yield(Value),
    /// A sync generator delegated to an inner iterator: the value is the
    /// inner iterator result object and is handed out as is.
    YieldRaw(Value),
    /// An async frame awaited (the frame has been saved).
    Await(Value),
    /// A generator frame finished its prologue (the frame has been saved).
    InitialYield,
}

/// What the dispatch loop should do after an instruction.
pub(crate) enum Flow {
    Next,
    /// The frame stack changed: reload the cached frame state.
    Reload,
    Done(RunOutcome),
}

impl Vm {
    /// Run until the top frame (an exit frame) completes.
    ///
    /// The running frame's `pc`, accumulator and register base live in
    /// locals here (ADR 0019 J1). They are written back to the frame before
    /// anything that can observe it (a call, a fault, a safe point, a
    /// debugger stop, a suspension): in practice before every instruction
    /// that leaves this loop for [`Vm::step`], `call_from_frame` or
    /// `do_return`, and before the attention slow path. Registers are
    /// addressed by base index because the register file may reallocate
    /// when a frame is pushed.
    ///
    /// Per instruction the loop tests one word, `Heap::attention` (J7). The
    /// host's interrupt hook is polled at function entry (here, and on
    /// every frame change), at loop back-edges and after a call returns.
    ///
    /// The arms below are the hot cases that allocate nothing, fault
    /// nothing and observe no frame; each falls back to `step` for the
    /// generic semantics (J3) of the same instruction.
    pub fn run(&mut self) -> JsResult<RunOutcome> {
        debug_assert!(self.frames.last().map(|f| f.exit).unwrap_or(false));
        self.refresh_attention();
        // Function entry: a host entry, a nested run loop or a resumed
        // generator. Frames pushed by calls poll on reload below.
        if let Err(fault) = self.poll_interrupt() {
            self.handle_fault(fault)?;
        }
        let mut fi = self.frames.len() - 1;
        let mut code = self.frames[fi].code.clone();
        let mut base = self.frames[fi].base;
        let mut pc = self.frames[fi].pc;
        let mut acc = self.frames[fi].acc;

        macro_rules! reg {
            ($r:expr) => {
                self.regs[base + ($r) as usize]
            };
        }
        macro_rules! writeback {
            () => {{
                let f = &mut self.frames[fi];
                f.pc = pc;
                f.acc = acc;
            }};
        }
        macro_rules! reload {
            () => {{
                fi = self.frames.len() - 1;
                let f = &self.frames[fi];
                if !Rc::ptr_eq(&code, &f.code) {
                    code = f.code.clone();
                }
                base = f.base;
                pc = f.pc;
                acc = f.acc;
            }};
        }
        macro_rules! fault {
            ($fault:expr) => {{
                writeback!();
                match self.handle_fault($fault) {
                    Ok(()) => {
                        reload!();
                        continue;
                    }
                    Err(f) => return Err(f),
                }
            }};
        }
        macro_rules! flow {
            ($flow:expr) => {
                match $flow {
                    Ok(Flow::Next) => {
                        let f = &self.frames[fi];
                        pc = f.pc;
                        acc = f.acc;
                    }
                    Ok(Flow::Reload) => {
                        reload!();
                        // Function entry, or a call returned (J7).
                        if let Err(f) = self.poll_interrupt() {
                            fault!(f);
                        }
                    }
                    Ok(Flow::Done(outcome)) => return Ok(outcome),
                    Err(f) => fault!(f),
                }
            };
        }
        // The generic path: the frame sees `pc` past the instruction and
        // the current accumulator, exactly as `step` expects.
        macro_rules! slow {
            ($instr:expr) => {{
                writeback!();
                flow!(self.step($instr, fi, base, &code));
            }};
        }
        macro_rules! jump {
            ($off:expr) => {{
                let off = $off;
                if off <= 0 {
                    // A loop back-edge (J7).
                    if let Err(f) = self.poll_interrupt() {
                        fault!(f);
                    }
                }
                pc = (pc as i64 - 1 + off as i64) as usize;
            }};
        }

        loop {
            if self.heap.attention != 0 {
                writeback!();
                if let Err(fault) = self.attention_slow(fi) {
                    fault!(fault);
                }
                // A debugger pause may have run code in this frame.
                let f = &self.frames[fi];
                pc = f.pc;
                acc = f.acc;
            }
            let instr = code.instrs[pc];
            pc += 1;
            match instr {
                // ---- loads and moves ----
                Instr::LdaUndefined => acc = Value::Undefined,
                Instr::LdaNull => acc = Value::Null,
                Instr::LdaTrue => acc = Value::Bool(true),
                Instr::LdaFalse => acc = Value::Bool(false),
                Instr::LdaEmpty => acc = Value::Empty,
                Instr::LdaInt(i) => acc = Value::Number(i as f64),
                Instr::LdaConst(c) => match code.constants[c as usize] {
                    Constant::Number(n) => acc = Value::Number(n),
                    _ => slow!(instr),
                },
                Instr::Ldar(r) => acc = reg!(r),
                Instr::Star(r) => reg!(r) = acc,
                Instr::Mov { dst, src } => {
                    let v = reg!(src);
                    reg!(dst) = v;
                }
                Instr::LdaThis => acc = self.frames[fi].this,

                // ---- bindings ----
                Instr::LdaEnv { depth, slot } => {
                    let e = self.env_at(fi, depth);
                    match self.heap.env(e).as_decl() {
                        Some(d) => acc = d.slots[slot as usize],
                        None => slow!(instr),
                    }
                }
                Instr::StaEnv { depth, slot } => {
                    let e = self.env_at(fi, depth);
                    match self.heap.env_mut(e).as_decl_mut() {
                        Some(d) => d.slots[slot as usize] = acc,
                        None => slow!(instr),
                    }
                }
                Instr::CheckTdz(_) if !acc.is_empty() => {}
                Instr::LdaGlobal(name) => match self.load_global_cached(&code, name) {
                    Some(v) => acc = v,
                    None => slow!(instr),
                },
                Instr::StaGlobal { name, .. } => {
                    if !self.store_global_cached(&code, name, acc) {
                        slow!(instr);
                    }
                }

                // ---- property access: inline-cache hits ----
                Instr::GetNamed { obj, ic, .. } => match reg!(obj) {
                    Value::Object(oid) => match self.ic_get(&code, ic, oid) {
                        Some(v) => acc = v,
                        None => slow!(instr),
                    },
                    _ => slow!(instr),
                },
                Instr::SetNamed { obj, ic, .. } => {
                    let o = reg!(obj);
                    match o {
                        Value::Object(oid) if self.ic_set(&code, ic, oid, acc) => {}
                        _ => slow!(instr),
                    }
                }

                // ---- operators on numbers ----
                Instr::Add(r) => match (reg!(r), acc) {
                    (Value::Number(a), Value::Number(b)) => acc = Value::Number(a + b),
                    _ => slow!(instr),
                },
                Instr::Sub(r) | Instr::Mul(r) | Instr::Div(r) | Instr::Mod(r) => {
                    match (reg!(r), acc) {
                        (Value::Number(a), Value::Number(b)) => {
                            acc = Value::Number(number_arith(instr_arith(instr), a, b))
                        }
                        _ => slow!(instr),
                    }
                }
                Instr::Shl(r)
                | Instr::Shr(r)
                | Instr::UShr(r)
                | Instr::BitAnd(r)
                | Instr::BitOr(r)
                | Instr::BitXor(r) => match (reg!(r), acc) {
                    (Value::Number(a), Value::Number(b)) => {
                        acc = Value::Number(number_bitwise(instr_arith(instr), a, b))
                    }
                    _ => slow!(instr),
                },
                Instr::Lt(r) => match (reg!(r), acc) {
                    (Value::Number(a), Value::Number(b)) => acc = Value::Bool(a < b),
                    _ => slow!(instr),
                },
                Instr::Gt(r) => match (reg!(r), acc) {
                    (Value::Number(a), Value::Number(b)) => acc = Value::Bool(a > b),
                    _ => slow!(instr),
                },
                Instr::Le(r) => match (reg!(r), acc) {
                    (Value::Number(a), Value::Number(b)) => acc = Value::Bool(a <= b),
                    _ => slow!(instr),
                },
                Instr::Ge(r) => match (reg!(r), acc) {
                    (Value::Number(a), Value::Number(b)) => acc = Value::Bool(a >= b),
                    _ => slow!(instr),
                },
                Instr::StrictEq(r) => {
                    let l = reg!(r);
                    acc = Value::Bool(self.strict_equals(l, acc));
                }
                Instr::StrictNe(r) => {
                    let l = reg!(r);
                    acc = Value::Bool(!self.strict_equals(l, acc));
                }
                Instr::Not => acc = Value::Bool(!self.to_boolean(acc)),
                Instr::Inc => match acc {
                    Value::Number(n) => acc = Value::Number(n + 1.0),
                    _ => slow!(instr),
                },
                Instr::Dec => match acc {
                    Value::Number(n) => acc = Value::Number(n - 1.0),
                    _ => slow!(instr),
                },
                Instr::ToNumeric if matches!(acc, Value::Number(_)) => {}

                // ---- control flow ----
                Instr::Jump(off) => jump!(off),
                Instr::JumpIfTrue(off) => {
                    if self.to_boolean(acc) {
                        jump!(off);
                    }
                }
                Instr::JumpIfFalse(off) => {
                    if !self.to_boolean(acc) {
                        jump!(off);
                    }
                }
                Instr::JumpIfNullish(off) => {
                    if acc.is_nullish() {
                        jump!(off);
                    }
                }
                Instr::JumpIfNotNullish(off) => {
                    if !acc.is_nullish() {
                        jump!(off);
                    }
                }
                Instr::JumpIfUndefined(off) => {
                    if acc.is_undefined() {
                        jump!(off);
                    }
                }
                Instr::JumpIfNotUndefined(off) => {
                    if !acc.is_undefined() {
                        jump!(off);
                    }
                }
                Instr::JumpIfEmpty(off) => {
                    if acc.is_empty() {
                        jump!(off);
                    }
                }
                Instr::JumpIfNotEmpty(off) => {
                    if !acc.is_empty() {
                        jump!(off);
                    }
                }
                Instr::JumpIfObject(off) => {
                    if acc.is_object() {
                        jump!(off);
                    }
                }
                Instr::JumpIfStrictEq { reg, offset } => {
                    let l = reg!(reg);
                    if self.strict_equals(l, acc) {
                        jump!(offset);
                    }
                }

                // ---- calls and returns ----
                Instr::Call {
                    callee,
                    this,
                    args,
                    argc,
                } => {
                    writeback!();
                    let f = reg!(callee);
                    let t = reg!(this);
                    let start = base + args as usize;
                    let a = self.regs[start..start + argc as usize].to_vec();
                    flow!(self.call_from_frame(f, t, a));
                }
                Instr::Return => {
                    writeback!();
                    flow!(Ok(self.do_return()));
                }
                Instr::Nop => {}

                _ => slow!(instr),
            }
        }
    }

    /// The dispatch loop's slow path, taken before an instruction while
    /// the attention word is non-zero: the debugger hook, a collection at
    /// this safe point, and the memory limit. The frame has been written
    /// back; `frames[fi].pc` is the instruction about to run.
    #[cold]
    #[inline(never)]
    fn attention_slow(&mut self, fi: usize) -> JsResult<()> {
        if self.debug_hot {
            self.debug_check(fi);
        }
        // The stress interval is counted inside maybe_collect.
        self.maybe_collect();
        if let Some(limit) = self.heap.memory_limit {
            // Under a native frame (a job, a callback) the collector is
            // deferred, so everything allocated since the last collection is
            // still held: count it, or a loop inside a callback would grow
            // past the limit unchecked.
            if self.heap.live_bytes + self.heap.allocated_since_gc > limit {
                return Err(Fault::OutOfMemory);
            }
        }
        self.refresh_attention();
        Ok(())
    }

    /// Route a fault to the nearest handler, popping frames as needed.
    /// `Ok` means execution continues at a handler; `Err` means the exit
    /// frame was popped and the fault propagates to the Rust caller.
    pub(crate) fn handle_fault(&mut self, fault: Fault) -> JsResult<()> {
        if self.debug_hot {
            self.debug_fault(&fault);
        }
        loop {
            let fi = self.frames.len() - 1;
            let skip = std::mem::take(&mut self.uncatchable);
            if let (Fault::Throw(v), false) = (fault, skip) {
                let frame = &self.frames[fi];
                let throw_pc = frame.pc.saturating_sub(1);
                if let Some(h) = frame.code.handler_for(throw_pc).copied() {
                    self.unwind_env_to(fi, h.env_depth);
                    let frame = &mut self.frames[fi];
                    frame.pc = h.target as usize;
                    frame.acc = v;
                    return Ok(());
                }
            }
            let frame = self.pop_frame();
            self.note_unwound_frame(&fault, &frame);
            if let Some(g) = frame.generator {
                crate::interp::generator::mark_completed(self, g);
            }
            if frame.exit {
                return Err(fault);
            }
        }
    }

    /// Pop environments until the frame's depth is `depth`.
    pub(crate) fn unwind_env_to(&mut self, fi: usize, depth: u16) {
        let frame = &mut self.frames[fi];
        while frame.env_depth > depth {
            let e = frame.env.expect("environment depth out of sync");
            frame.env = self.heap.env(e).parent();
            frame.env_depth -= 1;
        }
    }

    pub(crate) fn pop_frame(&mut self) -> Frame {
        let frame = self.frames.pop().expect("pop on empty frame stack");
        self.regs.truncate(frame.base);
        if let Some(prev) = self.frames.last() {
            self.current_realm = prev.realm;
        }
        frame
    }

    /// Push a frame for `code` with the register file extended.
    pub(crate) fn push_frame(&mut self, mut frame: Frame) -> JsResult<()> {
        if self.frames.len() >= crate::vm::MAX_FRAMES {
            return Err(self.throw_range("Maximum call stack size exceeded"));
        }
        frame.base = self.regs.len();
        let n = frame.code.register_count as usize;
        self.regs.resize(self.regs.len() + n, Value::Undefined);
        self.current_realm = frame.realm;
        self.frames.push(frame);
        Ok(())
    }

    /// The environment `depth` levels out from the frame's innermost one.
    #[inline]
    pub(crate) fn env_at(&self, fi: usize, depth: u16) -> EnvId {
        let mut e = self.frames[fi]
            .env
            .expect("environment access with no environment");
        for _ in 0..depth {
            e = self
                .heap
                .env(e)
                .parent()
                .expect("environment depth out of range");
        }
        e
    }

    /// Run `code` as a script in the current realm.
    pub fn run_script(&mut self, code: Rc<CodeBlock>) -> JsResult<Value> {
        let realm = self.current_realm;
        let global_env = self.realm().global_env;
        let frame = Frame {
            code,
            pc: 0,
            base: 0,
            acc: Value::Undefined,
            env: Some(global_env),
            base_env: Some(global_env),
            env_depth: 0,
            this: Value::Object(self.realm().global),
            callee: None,
            new_target: Value::Undefined,
            args: Vec::new(),
            argc: 0,
            realm,
            exit: true,
            construct: ConstructKind::None,
            kind: FrameKind::Script,
            generator: None,
            module: None,
        };
        self.push_frame(frame)?;
        match self.run()? {
            RunOutcome::Return(v) => Ok(v),
            other => panic!("script run ended with {other:?}"),
        }
    }

    /// Source position of the instruction at `pc` in `code` as (line, column), 1-based.
    pub fn line_col(code: &CodeBlock, pc: usize) -> (u32, u32) {
        let off = code.offset_at(pc.saturating_sub(1)) as usize;
        let text: &str = &code.source.text;
        let mut line = 1u32;
        let mut col = 1u32;
        for (i, ch) in text.char_indices() {
            if i >= off {
                break;
            }
            if ch == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }
}
