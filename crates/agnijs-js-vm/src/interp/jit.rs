//! The VM's side of the baseline JIT (feature `jit`, TASK-101.3, ADR 0020).
//!
//! The dispatch loop offers the top frame to compiled code at function
//! entry, when a call returns into a frame, and at loop back-edges
//! ([`Vm::jit_run`]). A block is compiled once it has been offered
//! `JitState::threshold` times. Compiled code runs the frame in place and
//! hands every instruction it does not do inline to [`JitRt::slow`], which
//! is the interpreter's own generic semantics (`Vm::step`, after the same
//! inline-cache fast paths the loop tries).
//!
//! Calls never recurse on the Rust stack: an instruction that pushes a
//! frame makes compiled code return (`Exit::Frame`), and the callee runs
//! like any other frame — compiled or interpreted — until it returns into
//! the caller, which is offered to compiled code again at the pc after the
//! call. Faults propagate the same way: compiled code returns, and the
//! interpreter's fault handling unwinds as usual (to a handler in the same
//! frame, which the interpreter then runs).
//!
//! Compiled code is never entered while a debugger is attached or the
//! attention word is set, and leaves for the interpreter (`Exit::Interp`)
//! as soon as either becomes true, so the interpreter's own loop-head
//! handling of both (ADR 0019 J7, J9) sees every instruction boundary it
//! would have seen.

use std::rc::Rc;
use std::time::Duration;

use agnijs_js_compiler::bytecode::{CodeBlock, Constant, Instr, Tier};
use agnijs_js_jit::{Arith, Compiled, Exit, JitValue, Resume, Runtime};

use super::{Args, Flow, RunOutcome};
use crate::value::Value;
use crate::vm::{Fault, JsResult, Vm};

impl JitValue for Value {
    fn undefined() -> Self {
        Value::Undefined
    }
    fn null() -> Self {
        Value::Null
    }
    fn boolean(b: bool) -> Self {
        Value::Bool(b)
    }
    fn number(n: f64) -> Self {
        Value::Number(n)
    }
    fn empty() -> Self {
        Value::Empty
    }
}

// The layout compiled code assumes is the one `Value` declares.
const _: () = {
    use agnijs_js_jit::layout as l;
    assert!(Value::TAG_OFFSET == l::TAG_OFFSET && Value::PAYLOAD_OFFSET == l::PAYLOAD_OFFSET);
    assert!(Value::TAG_UNDEFINED == l::TAG_UNDEFINED && Value::TAG_NULL == l::TAG_NULL);
    assert!(Value::TAG_BOOL == l::TAG_BOOL && Value::TAG_NUMBER == l::TAG_NUMBER);
    assert!(Value::TAG_STRING == l::TAG_STRING && Value::TAG_SYMBOL == l::TAG_SYMBOL);
    assert!(Value::TAG_BIGINT == l::TAG_BIGINT && Value::TAG_OBJECT == l::TAG_OBJECT);
    assert!(Value::TAG_EMPTY == l::TAG_EMPTY);
    assert!(std::mem::size_of::<Value>() == l::SIZE);
};

/// When compiled code runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JitMode {
    /// Never (the default).
    Off,
    /// Blocks offered [`JitState::threshold`] times are compiled.
    On,
    /// Every eligible block is compiled the first time it is offered (for
    /// tests: the most compiled code, the least interpretation).
    Force,
}

/// Blocks are compiled after this many entries and back-edges in mode
/// [`JitMode::On`].
pub const HOT_THRESHOLD: u32 = 1000;

/// What the JIT has done in a VM.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct JitStats {
    /// Blocks compiled.
    pub compiled: u32,
    /// Blocks that reached the threshold and were not compilable.
    pub rejected: u32,
    /// Hot blocks left to the interpreter because compiled code would
    /// mostly call back into it ([`agnijs_js_jit::profitable`]).
    pub unprofitable: u32,
    /// Runs of compiled code.
    pub entries: u64,
    /// Machine code generated, in bytes.
    pub code_bytes: usize,
    /// Time spent compiling.
    pub compile_time: Duration,
}

pub(crate) struct JitState {
    pub(crate) mode: JitMode,
    threshold: u32,
    /// The fault or outcome compiled code exited with.
    fault: Option<Fault>,
    done: Option<RunOutcome>,
    pub(crate) stats: JitStats,
}

impl JitState {
    /// The mode from `AGNIJS_JIT` (`on`/`1`, `force`; anything else off).
    pub(crate) fn from_env() -> JitState {
        let mode = match std::env::var("AGNIJS_JIT").as_deref() {
            Ok("on") | Ok("1") => JitMode::On,
            Ok("force") => JitMode::Force,
            _ => JitMode::Off,
        };
        let mut s = JitState {
            mode: JitMode::Off,
            threshold: HOT_THRESHOLD,
            fault: None,
            done: None,
            stats: JitStats::default(),
        };
        s.set_mode(mode);
        s
    }

    fn set_mode(&mut self, mode: JitMode) {
        self.mode = mode;
        self.threshold = if mode == JitMode::Force { 0 } else { HOT_THRESHOLD };
    }
}

impl Drop for JitState {
    /// `AGNIJS_JIT_STATS=1` reports what the JIT did, for tuning.
    fn drop(&mut self) {
        if self.mode != JitMode::Off && std::env::var_os("AGNIJS_JIT_STATS").is_some() {
            eprintln!("agnijs jit: {:?}", self.stats);
        }
    }
}

/// What [`Vm::jit_run`] did with the top frame.
pub(crate) enum JitRun {
    /// Nothing: the interpreter's locals are still current.
    NotEntered,
    /// Compiled code ran; reload the top frame and continue interpreting.
    Reload,
    Done(RunOutcome),
    /// Reload the top frame, then handle the fault.
    Fault(Fault),
}

impl Vm {
    /// Choose when compiled code runs (overrides `AGNIJS_JIT`).
    pub fn set_jit_mode(&mut self, mode: JitMode) {
        self.jit.set_mode(mode);
    }

    pub fn jit_mode(&self) -> JitMode {
        self.jit.mode
    }

    pub fn jit_stats(&self) -> JitStats {
        self.jit.stats
    }

    /// Offer the top frame, written back, to compiled code: count the
    /// offer, compile the block when it is hot, and run compiled code from
    /// the frame's pc while it can (through calls and returns, which leave
    /// and re-enter it).
    #[inline]
    pub(crate) fn jit_run(&mut self) -> JitRun {
        if self.jit.mode == JitMode::Off {
            return JitRun::NotEntered;
        }
        self.jit_run_slow()
    }

    /// Whether `code` may yet run compiled: the JIT is on and the block
    /// has not been left to the interpreter for good.
    #[inline]
    pub(crate) fn jit_live(&self, code: &CodeBlock) -> bool {
        self.jit.mode != JitMode::Off
            && code
                .feedback_if(self.feedback_owner)
                .is_none_or(|f| f.tier.state.get() != Tier::NEVER)
    }

    #[inline(never)]
    fn jit_run_slow(&mut self) -> JitRun {
        let mut entered = false;
        loop {
            let idle = if entered {
                JitRun::Reload
            } else {
                JitRun::NotEntered
            };
            if self.debug_hot || self.heap.attention != 0 {
                return idle;
            }
            let fi = self.frames.len() - 1;
            // Settle the common case, a block left to the interpreter,
            // before paying for anything else.
            let never = match self.frames[fi].code.feedback(self.feedback_owner) {
                Some(f) => f.tier.state.get() == Tier::NEVER,
                None => true,
            };
            if never {
                return idle;
            }
            let code = self.frames[fi].code.clone();
            let pc = self.frames[fi].pc;
            let Some(compiled) = self.jit_code(&code) else {
                return idle;
            };
            if !compiled.is_entry(pc) {
                return idle;
            }
            entered = true;
            self.jit.stats.entries += 1;
            let acc = self.frames[fi].acc;
            let base = self.frames[fi].base;
            let out = compiled.enter(
                &mut JitRt {
                    vm: self,
                    fi,
                    base,
                    code: &code,
                },
                pc as u32,
                acc,
            );
            match out.exit {
                Exit::Return => {
                    let f = &mut self.frames[fi];
                    f.pc = out.pc as usize;
                    f.acc = out.acc;
                    match self.do_return() {
                        Flow::Done(outcome) => return JitRun::Done(outcome),
                        _ => {
                            // A call returned (J7).
                            if let Err(f) = self.poll_interrupt() {
                                return JitRun::Fault(f);
                            }
                        }
                    }
                }
                Exit::Frame => {
                    // Function entry (J7).
                    if let Err(f) = self.poll_interrupt() {
                        return JitRun::Fault(f);
                    }
                }
                Exit::Fault => {
                    let f = self.jit.fault.take().expect("compiled code exited with a fault");
                    return JitRun::Fault(f);
                }
                Exit::Done => {
                    let o = self.jit.done.take().expect("compiled code exited with an outcome");
                    return JitRun::Done(o);
                }
                Exit::Interp => return JitRun::Reload,
            }
        }
    }

    /// The compiled code of `code`, compiling it if it has become hot.
    fn jit_code<'c>(&mut self, code: &'c CodeBlock) -> Option<&'c Compiled<Value>> {
        // A script body may have none yet: it is created on first use.
        let tier = &code.feedback(self.feedback_owner)?.tier;
        match tier.state.get() {
            Tier::COMPILED => tier.code.get()?.downcast_ref(),
            Tier::NEVER => None,
            _ => {
                let h = tier.hotness.get().saturating_add(1);
                tier.hotness.set(h);
                if h <= self.jit.threshold {
                    return None;
                }
                if self.jit.mode == JitMode::On && !agnijs_js_jit::profitable(code) {
                    self.jit.stats.unprofitable += 1;
                    tier.state.set(Tier::NEVER);
                    return None;
                }
                match Compiled::<Value>::compile(code) {
                    Ok(c) => {
                        let s = &mut self.jit.stats;
                        s.compiled += 1;
                        s.code_bytes += c.code_size;
                        s.compile_time += c.compile_time;
                        tier.state.set(Tier::COMPILED);
                        let _ = tier.code.set(Rc::new(c));
                        tier.code.get()?.downcast_ref()
                    }
                    Err(_) => {
                        self.jit.stats.rejected += 1;
                        tier.state.set(Tier::NEVER);
                        None
                    }
                }
            }
        }
    }

    /// The dispatch loop's fast paths for an instruction compiled code
    /// hands over: inline-cache hits, dense elements, cached bindings.
    /// They observe no frame, allocate nothing and cannot fault, so the
    /// frame is not written back for them. `Some` is the new accumulator.
    #[inline(always)]
    fn jit_fast(&mut self, instr: Instr, fi: usize, base: usize, code: &CodeBlock, acc: Value) -> Option<Value> {
        match instr {
            Instr::GetNamed { obj, ic, .. } => match self.ic_get(code, ic, self.regs[base + obj as usize]) {
                Some(super::ic::IcGet::Value(v)) => Some(v),
                _ => None,
            },
            Instr::SetNamed { obj, ic, .. } => {
                let target = self.regs[base + obj as usize];
                match target {
                    Value::Object(oid) if self.ic_set(code, ic, oid, acc) => Some(acc),
                    _ => None,
                }
            }
            Instr::GetKeyed { obj, key } => match (self.regs[base + obj as usize], self.regs[base + key as usize]) {
                (Value::Object(oid), Value::Number(n)) => self.keyed_get_fast(oid, n),
                _ => None,
            },
            Instr::SetKeyed { obj, key, .. } => {
                let target = (self.regs[base + obj as usize], self.regs[base + key as usize]);
                match target {
                    (Value::Object(oid), Value::Number(n)) if self.keyed_set_fast(oid, n, acc) => Some(acc),
                    _ => None,
                }
            }
            Instr::LdaGlobal(name) => self.load_global_cached(code, name),
            Instr::StaGlobal { name, .. } => self.store_global_cached(code, name, acc).then_some(acc),
            Instr::LdaEnv { depth, slot } => {
                let e = self.env_at(fi, depth);
                self.heap.env(e).as_decl().map(|d| d.slots[slot as usize])
            }
            Instr::StaEnv { depth, slot } => {
                let e = self.env_at(fi, depth);
                let d = self.heap.env_mut(e).as_decl_mut()?;
                d.slots[slot as usize] = acc;
                Some(acc)
            }
            Instr::LdaThis => Some(self.frames[fi].this),
            Instr::LdaConst(c) => match code.constants[c as usize] {
                Constant::String(_) => match code.feedback_if(self.feedback_owner) {
                    Some(f) if f.strings[c as usize].get().epoch == self.gc_count => {
                        Some(Value::String(crate::value::StrId(f.strings[c as usize].get().id)))
                    }
                    _ => None,
                },
                _ => None,
            },
            Instr::StrictEq(r) => Some(Value::Bool(self.strict_equals(self.regs[base + r as usize], acc))),
            Instr::StrictNe(r) => Some(Value::Bool(!self.strict_equals(self.regs[base + r as usize], acc))),
            Instr::Not => Some(Value::Bool(!self.to_boolean(acc))),
            _ => None,
        }
    }

    /// One instruction by its generic semantics, `frames[fi]` written
    /// back (calls take the dispatch loop's direct path).
    #[inline(never)]
    fn jit_generic(&mut self, instr: Instr, fi: usize, base: usize, code: &Rc<CodeBlock>) -> JsResult<Flow> {
        if let Instr::Call {
            callee,
            this,
            args,
            argc,
        } = instr
        {
            let f = self.regs[base + callee as usize];
            let t = self.regs[base + this as usize];
            let a = Args::Regs {
                start: base + args as usize,
                len: argc as usize,
            };
            return self.call_from_frame(f, t, a);
        }
        self.step(instr, fi, base, code)
    }
}

/// The runtime compiled code calls back into: the VM running frame `fi`.
struct JitRt<'a> {
    vm: &'a mut Vm,
    fi: usize,
    base: usize,
    code: &'a Rc<CodeBlock>,
}

impl JitRt<'_> {
    /// Where execution goes after an instruction run by the runtime.
    #[inline(never)]
    fn after(&mut self, flow: JsResult<Flow>) -> Resume<Value> {
        let vm = &mut *self.vm;
        match flow {
            Ok(Flow::Next) if vm.frames.len() == self.fi + 1 => {
                if vm.debug_hot || vm.heap.attention != 0 {
                    // The interpreter's loop head handles both before the
                    // next instruction.
                    return Resume::Exit(Exit::Interp);
                }
                let f = &vm.frames[self.fi];
                Resume::Next {
                    pc: f.pc as u32,
                    acc: f.acc,
                }
            }
            Ok(Flow::Next) | Ok(Flow::Reload) => Resume::Exit(Exit::Frame),
            Ok(Flow::Done(outcome)) => {
                vm.jit.done = Some(outcome);
                Resume::Exit(Exit::Done)
            }
            Err(fault) => {
                vm.jit.fault = Some(fault);
                Resume::Exit(Exit::Fault)
            }
        }
    }

    #[inline(never)]
    fn generic(&mut self, pc: usize, instr: Instr, acc: Value) -> Resume<Value> {
        let f = &mut self.vm.frames[self.fi];
        f.pc = pc + 1;
        f.acc = acc;
        let flow = self.vm.jit_generic(instr, self.fi, self.base, self.code);
        self.after(flow)
    }
}

impl Runtime<Value> for JitRt<'_> {
    fn slow(&mut self, pc: u32, acc: Value) -> Resume<Value> {
        let pc = pc as usize;
        let instr = self.code.instrs[pc];
        if let Some(v) = self.vm.jit_fast(instr, self.fi, self.base, self.code, acc) {
            if self.vm.heap.attention == 0 && !self.vm.debug_hot {
                return Resume::Next {
                    pc: pc as u32 + 1,
                    acc: v,
                };
            }
            let f = &mut self.vm.frames[self.fi];
            f.pc = pc + 1;
            f.acc = v;
            return Resume::Exit(Exit::Interp);
        }
        self.generic(pc, instr, acc)
    }

    fn poll(&mut self, pc: u32, target: u32, acc: Value) -> Resume<Value> {
        let f = &mut self.vm.frames[self.fi];
        f.pc = pc as usize + 1;
        f.acc = acc;
        // Compiled code advanced the counter; the poll itself is due.
        let flow = self.vm.ask_host_interrupt().map(|()| {
            self.vm.frames[self.fi].pc = target as usize;
            Flow::Next
        });
        self.after(flow)
    }

    fn registers(&mut self) -> &mut [Value] {
        &mut self.vm.regs[self.base..]
    }

    fn interrupt_counter(&self) -> u32 {
        self.vm.interrupt_counter
    }

    fn set_interrupt_counter(&mut self, counter: u32) {
        self.vm.interrupt_counter = counter;
    }

    fn arith(op: Arith, a: f64, b: f64) -> f64 {
        match op {
            Arith::Mod => crate::convert::js_fmod(a, b),
            Arith::Exp => crate::convert::js_pow(a, b),
        }
    }
}
