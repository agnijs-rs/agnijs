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
//! would have seen. A fuel budget (TASK-103.1) is the same: it sets the
//! attention word and keeps compiled code out, so every instruction is
//! counted by the interpreter and the count does not depend on the tier.

use std::rc::Rc;
use std::time::Duration;

use agnijs_js_compiler::bytecode::{CodeBlock, Constant, IcSite, Instr, Tier};
use agnijs_js_jit::{Arith, Compiled, Exit, HeapView, JitValue, Resume, Runtime};

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

/// In mode [`JitMode::On`] a block must also have taken this many loop
/// back-edges per instruction before it is compiled: compiling costs about
/// 25 µs per bytecode instruction (Cranelift, about 20 IR instructions
/// each), what interpreting some 10 000 instructions costs, so a block is
/// compiled only once its loops look like they will run long enough to
/// pay that back (Gable's ui-bundle-startup lost its compile time).
pub const HOT_PER_INSTRUCTION: u32 = 200;

#[inline(always)]
fn hot_for(code: &CodeBlock) -> u32 {
    (code.instrs.len() as u32).saturating_mul(HOT_PER_INSTRUCTION)
}

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
    /// Compiled blocks given back to the interpreter in mode `On` because
    /// their loops called out on too many instructions as they ran.
    pub abandoned: u32,
    /// Runs of compiled code started by the tier loop ([`Vm::jit_run`]).
    pub entries: u64,
    /// Calls (and getter or setter runs) whose callee's compiled code was
    /// entered from the caller's compiled code, and returned into it,
    /// without going back to the tier loop.
    pub direct_calls: u64,
    /// Loop back-edges taken by compiled code.
    pub back_edges: u64,
    /// Calls from compiled code into the runtime ([`Runtime::slow`] and
    /// [`Runtime::poll`]): what compiled code could not do inline.
    pub calls_out: u64,
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
    /// Mode `On`'s window over the block last judged at a poll.
    judge: Judge,
    /// The interrupt counter when the current run was entered.
    run_start: u32,
    /// Compiled callees running inside a call out of their callers'
    /// compiled code ([`JitRt::enter_callee`]), bounded by [`MAX_NEST`].
    nest: u32,
}

/// How deep compiled code may nest on the native stack through direct
/// calls. A call past it leaves compiled code as before (`Exit::Frame`),
/// which unwinds the nested machine frames to the tier loop; their frames
/// are interpreter frames already, so they continue from memory. A level
/// measured 1.2 KB of native stack in a release build and 1.7 KB in a
/// debug one, so the bound adds at most about 110 KB to the stack the
/// interpreter's own native depth limit allows; the bound is per engine,
/// not per run, so native re-entry does not multiply it.
pub const MAX_NEST: u32 = 64;

/// The block (by address) a judgement window is over, and the counts when
/// it opened: how often runs of that block call out, and are entered, per
/// back-edge, across runs (a loop that calls a function leaves compiled
/// code and re-enters it on every call).
#[derive(Clone, Copy, Default)]
struct Judge {
    block: usize,
    calls_out: u64,
    entries: u64,
    edges: u64,
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
            judge: Judge::default(),
            run_start: 0,
            nest: 0,
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
    /// `back_edge`: the offer is at a loop back-edge (else at function
    /// entry or a call's return).
    #[inline]
    pub(crate) fn jit_run(&mut self, back_edge: bool) -> JitRun {
        if self.jit.mode == JitMode::Off || self.counted(back_edge) {
            return JitRun::NotEntered;
        }
        self.jit_run_slow()
    }

    /// The common offer while a block is warming up, counted without
    /// leaving the dispatch loop: it is not yet hot, or (mode `On`) is
    /// judged worth compiling but has not yet looped enough to pay for it
    /// ([`HOT_PER_INSTRUCTION`]; only back-edges count towards that). False
    /// when [`Vm::jit_run_slow`] has to look (and counts the offer itself).
    #[inline(always)]
    fn counted(&self, back_edge: bool) -> bool {
        if self.heap.attention != 0 || self.debug_hot {
            return false;
        }
        let Some(fr) = self.frames.last() else {
            return false;
        };
        let Some(f) = fr.code.feedback_if(self.feedback_owner) else {
            return false;
        };
        let tier = &f.tier;
        if tier.state.get() != Tier::COLD {
            return false;
        }
        let seen = tier.hotness.get();
        let t = self.jit.threshold;
        if seen < t {
            tier.hotness.set(seen + 1);
            return true;
        }
        if self.jit.mode != JitMode::On || seen == t {
            // Hot: judged by the slow path.
            return false;
        }
        // Judged worth compiling, warming up on its loops.
        if !back_edge {
            return true;
        }
        let h = seen.saturating_add(1);
        let waiting = h <= hot_for(&fr.code);
        if waiting {
            tier.hotness.set(h);
        }
        waiting
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
            if self.debug_hot || self.fuel_on {
                return idle;
            }
            let fi = self.frames.len() - 1;
            if self.heap.attention != 0 {
                // A collection (or the memory limit) is due: this is an
                // instruction boundary with the frame written back, the
                // safe point the interpreter's loop head would make it.
                // Under GC stress the word stays set, and compiled code
                // still runs.
                if let Err(f) = self.attention_slow(fi) {
                    return JitRun::Fault(f);
                }
            }
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
            // Runs that end soon after they start (the block's loops run
            // few times per call, or call out) are judged as they begin,
            // by the whole block.
            if self.unprofitable_runs(&code, code.instrs.len() as u64, false) {
                if let Some(f) = code.feedback_if(self.feedback_owner) {
                    f.tier.state.set(Tier::NEVER);
                }
                self.jit.stats.abandoned += 1;
                return idle;
            }
            entered = true;
            self.jit.stats.entries += 1;
            self.jit.run_start = self.interrupt_counter;
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
            self.jit.stats.back_edges += u64::from(self.interrupt_counter.wrapping_sub(self.jit.run_start));
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

    /// Mode `On`'s check of its static estimate ([`agnijs_js_jit::profitable`])
    /// against what runs of `code` did since the window over it opened:
    /// they called out more than once per [`agnijs_js_jit::CALL_OUT_SHARE`]
    /// instructions of `body` per back-edge, an entry (a call returning to
    /// the block, or a call of it) counting as [`ENTRY_SHARE`] call-outs.
    /// (Instructions the estimate counts as inline, such as `+`, call out
    /// when their operands are not numbers.) The window spans runs, since a
    /// loop that calls is a new run after every call, and is judged after
    /// [`JUDGE_AFTER`] back-edges or entries.
    fn unprofitable_runs(&mut self, code: &Rc<CodeBlock>, body: u64, running: bool) -> bool {
        if self.jit.mode != JitMode::On {
            return false;
        }
        let now = Judge {
            block: Rc::as_ptr(code) as usize,
            calls_out: self.jit.stats.calls_out,
            entries: self.jit.stats.entries,
            edges: self.jit.stats.back_edges + if running { u64::from(self.interrupt_counter.wrapping_sub(self.jit.run_start)) } else { 0 },
        };
        let open = self.jit.judge;
        if open.block != now.block {
            self.jit.judge = now;
            return false;
        }
        let edges = now.edges - open.edges;
        let entries = now.entries - open.entries;
        if edges < u64::from(JUDGE_AFTER) && entries < u64::from(JUDGE_AFTER) {
            return false;
        }
        self.jit.judge = now;
        let calls = (now.calls_out - open.calls_out) + entries * ENTRY_SHARE;
        calls * agnijs_js_jit::CALL_OUT_SHARE as u64 > body * edges
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
                let layout = crate::object::jit_heap_layout();
                if self.jit.mode == JitMode::On {
                    // Judged when it first becomes hot, so that a block not
                    // worth compiling stops being counted, and again when
                    // it is hot enough to compile, by its feedback then.
                    let ready = h > hot_for(code);
                    if (ready || h == self.jit.threshold + 1)
                        && !agnijs_js_jit::profitable_with(code, layout.is_some())
                    {
                        self.jit.stats.unprofitable += 1;
                        tier.state.set(Tier::NEVER);
                        return None;
                    }
                    if !ready {
                        return None;
                    }
                }
                match Compiled::<Value>::compile_with(code, layout.as_ref()) {
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
            // (Not `GetNamed` or `SetNamed`: compiled code probed their
            // inline cache already, and the generic path probes it first,
            // so a miss, the common reason to be here, would probe it
            // three times; a megamorphic site in Gable's signal-churn lost
            // a tenth of its time to that.)
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

/// What a call from compiled code costs the caller's loop, in call-outs:
/// leaving compiled code, the function-entry and return polls, and a new
/// run after the return that starts from the frame in memory. Set where a
/// loop of calls (Gable's signal-churn) stops losing to the interpreter;
/// the round trip alone measures about 3 call-outs.
const ENTRY_SHARE: u64 = 20;

/// Back-edges a block's runs must take before its loops are judged.
const JUDGE_AFTER: u32 = 1024;

impl JitRt<'_> {
    /// Where execution goes after an instruction run by the runtime.
    #[inline(never)]
    fn after(&mut self, flow: JsResult<Flow>) -> Resume<Value> {
        let vm = &mut *self.vm;
        match flow {
            Ok(Flow::Next) if vm.frames.len() == self.fi + 1 => {
                if vm.debug_hot {
                    // The interpreter's loop head handles the debugger
                    // before the next instruction.
                    return Resume::Exit(Exit::Interp);
                }
                if vm.heap.attention != 0 {
                    return self.attention();
                }
                let f = &vm.frames[self.fi];
                Resume::Next {
                    pc: f.pc as u32,
                    acc: f.acc,
                }
            }
            Ok(Flow::Next) | Ok(Flow::Reload) if vm.frames.len() == self.fi + 2 => self.enter_callee(),
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

    /// The instruction just run (a call, `new`, a getter or setter) pushed
    /// one frame over this one: run the callee's compiled code from here,
    /// inside this call out, and continue this function's compiled code
    /// when it returns (TASK-101.3.3). Nothing of the caller is outside its
    /// frame (it was written back for the call out), so the caller's
    /// machine frame below holds no value the collector cannot see and no
    /// state the interpreter frame lacks: no GC map and no deoptimization.
    ///
    /// The callee's frame was pushed by the interpreter's own path (the
    /// stack-depth check included); this does what the tier loop does for
    /// it: the function-entry poll (J7) and the attention safe point, then
    /// a run, then on `Return` the interpreter's `do_return` and the poll
    /// after a call returns. Any other end of the callee's run (it pushed a
    /// frame it did not run, faulted, finished the run, or wants the
    /// interpreter) unwinds this caller's machine code too, to the tier
    /// loop, which carries on from the frames as they are.
    ///
    /// A callee that has no compiled code yet, a debugger, or the nesting
    /// bound leave the old way (`Exit::Frame`), before the poll, so the
    /// tier loop polls and counts the entry as it always has.
    #[inline(never)]
    fn enter_callee(&mut self) -> Resume<Value> {
        let callee = self.fi + 1;
        let vm = &mut *self.vm;
        if vm.debug_hot || vm.fuel_on || vm.jit.nest >= MAX_NEST {
            return Resume::Exit(Exit::Frame);
        }
        let code = vm.frames[callee].code.clone();
        let pc = vm.frames[callee].pc;
        // Already compiled: a callee still being counted towards its
        // threshold is counted by the tier loop, once.
        let compiled = match code.feedback_if(vm.feedback_owner) {
            Some(f) if f.tier.state.get() == Tier::COMPILED => f.tier.code.get().and_then(|c| c.downcast_ref::<Compiled<Value>>()),
            _ => None,
        };
        let Some(compiled) = compiled else {
            return Resume::Exit(Exit::Frame);
        };
        if !compiled.is_entry(pc) {
            return Resume::Exit(Exit::Frame);
        }
        let c0 = vm.interrupt_counter;
        let outer_start = vm.jit.run_start;
        let r = Self::run_callee(vm, callee, &code, compiled, pc);
        // The caller's run does not count the callee's back-edges or the
        // polls made for it.
        vm.jit.run_start = outer_start.wrapping_add(vm.interrupt_counter.wrapping_sub(c0));
        match r {
            Ok(Ok(())) => {
                // `after(Ok(Flow::Next))`, its common case here.
                if !vm.debug_hot && vm.heap.attention == 0 {
                    let f = &vm.frames[self.fi];
                    return Resume::Next {
                        pc: f.pc as u32,
                        acc: f.acc,
                    };
                }
                self.after(Ok(Flow::Next))
            }
            Ok(Err(exit)) => Resume::Exit(exit),
            Err(fault) => {
                vm.jit.fault = Some(fault);
                Resume::Exit(Exit::Fault)
            }
        }
    }

    /// [`JitRt::enter_callee`]'s run of the callee at `frames[callee]`,
    /// from its entry `pc` to its return into the caller's frame: `Ok(Err)`
    /// is an end that unwinds the caller's machine code too.
    #[inline(always)]
    fn run_callee(
        vm: &mut Vm,
        callee: usize,
        code: &Rc<CodeBlock>,
        compiled: &Compiled<Value>,
        pc: usize,
    ) -> JsResult<Result<(), Exit>> {
        // Function entry (J7), then the safe point at the callee's first
        // instruction, as the tier loop makes them.
        vm.poll_interrupt()?;
        if vm.heap.attention != 0 {
            vm.attention_slow(callee)?;
        }
        vm.jit.nest += 1;
        vm.jit.stats.direct_calls += 1;
        vm.jit.run_start = vm.interrupt_counter;
        let f = &vm.frames[callee];
        let (acc, base) = (f.acc, f.base);
        let out = compiled.enter(
            &mut JitRt {
                vm: &mut *vm,
                fi: callee,
                base,
                code,
            },
            pc as u32,
            acc,
        );
        vm.jit.nest -= 1;
        // (`run_start` has been moved past the polls of calls nested in
        // this one, which counted their own.)
        vm.jit.stats.back_edges += u64::from(vm.interrupt_counter.wrapping_sub(vm.jit.run_start));
        if out.exit != Exit::Return {
            return Ok(Err(out.exit));
        }
        let f = &mut vm.frames[callee];
        f.pc = out.pc as usize;
        f.acc = out.acc;
        if let Flow::Done(outcome) = vm.do_return() {
            vm.jit.done = Some(outcome);
            return Ok(Err(Exit::Done));
        }
        // A call returned (J7).
        vm.poll_interrupt()?;
        if vm.frames.len() != callee {
            return Ok(Err(Exit::Interp));
        }
        Ok(Ok(()))
    }

    /// The safe point after an instruction, frame written back, when the
    /// attention word asks for a collection (or the memory-limit check):
    /// what the interpreter's loop head does before the next instruction.
    /// Any collection runs here, under compiled code, which holds nothing
    /// of the heap across this call: its registers and accumulator are in
    /// the frame, and it reloads the arena base after it.
    #[cold]
    #[inline(never)]
    fn attention(&mut self) -> Resume<Value> {
        let vm = &mut *self.vm;
        if vm.fuel_on {
            // A host call set a budget: the interpreter counts from the
            // next instruction.
            return Resume::Exit(Exit::Interp);
        }
        if let Err(fault) = vm.attention_slow(self.fi) {
            vm.jit.fault = Some(fault);
            return Resume::Exit(Exit::Fault);
        }
        if vm.frames.len() != self.fi + 1 {
            return Resume::Exit(Exit::Frame);
        }
        let f = &vm.frames[self.fi];
        Resume::Next {
            pc: f.pc as u32,
            acc: f.acc,
        }
    }

    /// [`Vm::unprofitable_runs`] at an interrupt poll on the back-edge from
    /// `pc` to `target`, judged by that loop's body.
    fn unprofitable_loop(&mut self, pc: u32, target: u32) -> bool {
        let body = u64::from(pc.saturating_sub(target)) + 1;
        self.vm.unprofitable_runs(self.code, body, true)
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
        self.vm.jit.stats.calls_out += 1;
        let pc = pc as usize;
        let instr = self.code.instrs[pc];
        if let Some(v) = self.vm.jit_fast(instr, self.fi, self.base, self.code, acc) {
            if self.vm.heap.attention == 0 {
                return Resume::Next {
                    pc: pc as u32 + 1,
                    acc: v,
                };
            }
            let f = &mut self.vm.frames[self.fi];
            f.pc = pc + 1;
            f.acc = v;
            if self.vm.debug_hot {
                return Resume::Exit(Exit::Interp);
            }
            return self.attention();
        }
        self.generic(pc, instr, acc)
    }

    fn this_value(&mut self) -> Option<Value> {
        Some(self.vm.frames[self.fi].this)
    }

    /// A cached global, a declarative environment slot or the callee: the hit
    /// in a small function; a miss, or a safe point due, by [`Runtime::slow`].
    fn fast(&mut self, pc: u32, acc: Value) -> Option<Value> {
        let vm = &mut *self.vm;
        if vm.heap.attention != 0 {
            return None;
        }
        let hit = match self.code.instrs[pc as usize] {
            Instr::LdaGlobal(name) => vm.load_global_cached(self.code, name),
            Instr::LdaEnv { depth, slot } => {
                let e = vm.env_at(self.fi, depth);
                vm.heap.env(e).as_decl().map(|d| d.slots[slot as usize])
            }
            Instr::StaEnv { depth, slot } => {
                let e = vm.env_at(self.fi, depth);
                vm.heap.env_mut(e).as_decl_mut().map(|d| {
                    d.slots[slot as usize] = acc;
                    acc
                })
            }
            Instr::LdaCallee => Some(vm.frames[self.fi].callee.map_or(Value::Undefined, Value::Object)),
            _ => None,
        };
        vm.jit.stats.calls_out += 1;
        hit
    }

    /// `Call`, the dispatch loop's way (`call_from_frame`), without the
    /// general decode; a compiled callee's frame runs from here.
    fn call(&mut self, pc: u32, acc: Value) -> Resume<Value> {
        let pc = pc as usize;
        let Instr::Call {
            callee,
            this,
            args,
            argc,
        } = self.code.instrs[pc]
        else {
            return self.slow(pc as u32, acc);
        };
        let vm = &mut *self.vm;
        vm.jit.stats.calls_out += 1;
        let f = &mut vm.frames[self.fi];
        f.pc = pc + 1;
        f.acc = acc;
        let base = self.base;
        let fv = vm.regs[base + callee as usize];
        let t = vm.regs[base + this as usize];
        let a = Args::Regs {
            start: base + args as usize,
            len: argc as usize,
        };
        match vm.call_from_frame(fv, t, a) {
            Ok(Flow::Reload) if vm.frames.len() == self.fi + 2 => self.enter_callee(),
            flow => self.after(flow),
        }
    }

    fn heap(&mut self) -> HeapView {
        HeapView {
            objects: self.vm.heap.objects.base_ptr(),
            proto_epoch: self.vm.heap.proto_epoch,
        }
    }

    fn ic_table(&mut self) -> &[IcSite] {
        match self.code.feedback_if(self.vm.feedback_owner) {
            Some(f) => {
                // Entries from before a collection that freed shapes name
                // shapes that may since have been reused.
                self.vm.fresh_ic(f);
                &f.ic
            }
            None => &[],
        }
    }

    fn poll(&mut self, pc: u32, target: u32, acc: Value) -> Resume<Value> {
        self.vm.jit.stats.calls_out += 1;
        let f = &mut self.vm.frames[self.fi];
        f.pc = pc as usize + 1;
        f.acc = acc;
        // Compiled code advanced the counter; the host poll is due when it
        // reached a multiple of 0x1000, else only the safe point is (GC
        // attention under stress), which `after` services.
        let ask = if self.vm.interrupt_counter & 0xfff == 0 {
            let asked = self.vm.ask_host_interrupt();
            if asked.is_ok() && self.unprofitable_loop(pc, target) {
                // Continue at the loop head in the interpreter, for good.
                self.vm.frames[self.fi].pc = target as usize;
                if let Some(fb) = self.code.feedback_if(self.vm.feedback_owner) {
                    fb.tier.state.set(Tier::NEVER);
                }
                self.vm.jit.stats.abandoned += 1;
                return Resume::Exit(Exit::Interp);
            }
            asked
        } else {
            Ok(())
        };
        let flow = ask.map(|()| {
            self.vm.frames[self.fi].pc = target as usize;
            Flow::Next
        });
        self.after(flow)
    }

    fn safe_point_due(&self) -> bool {
        self.vm.heap.attention != 0
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
