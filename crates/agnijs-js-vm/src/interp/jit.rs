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
//! A call stays in compiled code where it can (TASK-101.3.3). A `Call` of
//! a function in the VM's callee table is made by machine code itself: it
//! pushes the callee's `Frame`, calls the callee's machine code on the
//! native stack and pops the frame (`CallLayout`, [`Vm::jit_add_callee`]).
//! Any other instruction that pushes one frame (a call machine code could
//! not make, `new`, a getter or setter) runs that frame from inside the
//! call out: its compiled code ([`JitRt::enter_callee`]) or, when it has
//! none, the interpreter's loop ([`JitRt::interpret_callee`]). Only past
//! the native stack bound ([`JIT_STACK`]), or with a debugger or a fuel
//! budget, does compiled code return instead (`Exit::Frame`); the callee
//! then runs like any other frame until it returns into the caller, which
//! is offered to compiled code again at the pc after the call. Faults
//! leave compiled code: it returns, level by level, and the interpreter's
//! fault handling unwinds as usual (to a handler in the same frame, which
//! the interpreter then runs).
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
use agnijs_js_jit::{
    Arith, CallLayout, CallView, Callee, CalleeTable, Compiled, DeoptReason, EnvLayout, Exit,
    FrameHandle, FrameState, GlobalSite, HeapView, JitValue, Resume, Runtime, Speculation,
};
use agnijs_js_parser::ast::FunctionKind;

use super::{Args, ConstructKind, Flow, Frame, FrameKind, RunOutcome};
use crate::object::ThisMode;
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
    /// Blocks compiled (a block recompiled after a guess failed counts
    /// again).
    pub compiled: u32,
    /// Compilations that guessed some register holds only numbers
    /// (TASK-101.3.1).
    pub speculated: u32,
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
    /// Calls from compiled code of a function without compiled code, run
    /// in the interpreter from inside the call out.
    pub interpreted: u64,
    /// Calls (and getter or setter runs) whose callee's compiled code was
    /// entered from the caller's compiled code, and returned into it,
    /// without going back to the tier loop: by machine code itself (a
    /// `Call` whose site names the callee) or from inside the call out.
    pub direct_calls: u64,
    /// Loop back-edges taken by compiled code, and the polls of the calls
    /// it made directly (two each: at the callee's entry and after its
    /// return): the work compiled code did without leaving.
    pub back_edges: u64,
    /// Side exits: compiled code left for the interpreter before an
    /// instruction because a guard failed (TASK-101.3.1).
    pub deopts: u64,
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
    /// The lowest address the native stack pointer may be at when
    /// compiled code nests further ([`JIT_STACK`] below where it was when
    /// compiled code was first entered with none running): past it a call
    /// leaves compiled code as it used to (`Exit::Frame`).
    stack_floor: usize,
    /// The functions machine code may call directly, by code block.
    callees: CalleeTable,
    /// What entries of `callees` kept alive before another callee
    /// replaced them ([`CalleeTable::fill`]): dropped when no compiled
    /// code is running, since a frame of the old callee may be below the
    /// call that replaced it.
    retired: Vec<Box<dyn std::any::Any>>,
    /// What machine code does itself (TASK-101.3.3), a bit each: calls
    /// ([`DIRECT_CALLS`]), environment slots ([`DIRECT_ENVS`]) and global
    /// bindings ([`DIRECT_GLOBALS`]). All by default; `AGNIJS_JIT_CALLS`
    /// gives another set (0 for none), to measure each or to look for a
    /// fault in one.
    direct: u32,
    /// The compiled frames that exist, innermost last (TASK-101.3.1): the
    /// collector traces what each holds outside the register file through
    /// its handle, and a debugger attaching writes it into the interpreter
    /// frame. Every one has an interpreter frame at `fi`.
    pub(crate) frames: Vec<JitFrame>,
}

/// One running compiled frame ([`JitState::frames`]).
pub(crate) struct JitFrame {
    pub(crate) handle: FrameHandle<Value>,
    pub(crate) fi: usize,
    pub(crate) base: usize,
    /// Its state has been written into the interpreter frame already (a
    /// debugger attached while it waited in a call out), so the call out's
    /// own materialisation on leaving must not undo what was written since.
    pub(crate) materialized: bool,
}

/// How much native stack compiled code may nest over, in bytes: machine
/// frames of functions called directly, and the runs of compiled callees
/// inside call outs ([`JitRt::enter_callee`], about 1.2 KB a level in a
/// release build and 1.7 KB in a debug one). A call past it leaves
/// compiled code as before (`Exit::Frame`), which unwinds the nested
/// machine frames to the tier loop; their frames are interpreter frames
/// already, so they continue from memory. The bound is on the stack
/// pointer, so it holds whatever a machine frame's size, and it is per
/// engine, not per run: native re-entry does not multiply it (natives
/// under compiled code use it up instead). It adds at most this much to
/// the stack the interpreter's own native depth limit allows, plus one
/// more level, and never reaches past the host's stack budget
/// (`agnijs_js_parser::stack::limit`), which is the floor when it is the
/// nearer of the two. An unoptimised build has frames an order of magnitude
/// larger (its dispatch loop alone measured 58 KB, which a callee run in
/// the interpreter from a call out puts on the stack), so it gets more.
pub const JIT_STACK: usize = if cfg!(debug_assertions) {
    256 * 1024
} else {
    96 * 1024
};

/// What a compiled callee's run inside a call out needs of the stack
/// above the floor before it starts.
const NEST_STACK: usize = 4 * 1024;

/// The native stack pointer, near enough: the address of a local (stacks
/// grow downwards on every target the JIT supports).
#[inline(never)]
fn stack_position() -> usize {
    let marker = 0u8;
    std::hint::black_box(std::ptr::addr_of!(marker)) as usize
}

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
            stack_floor: 0,
            callees: CalleeTable::default(),
            retired: Vec::new(),
            direct: std::env::var("AGNIJS_JIT_CALLS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DIRECT_CALLS | DIRECT_ENVS | DIRECT_GLOBALS),
            frames: Vec::new(),
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

    /// Write `state` into the interpreter frame `fi` (window at `base`):
    /// the registers compiled code held outside the register file, and
    /// for a side exit the pc and accumulator to continue at. A frame that
    /// is gone (the run ended, an unwind popped it) takes nothing.
    pub(crate) fn jit_write_state(&mut self, fi: usize, base: usize, state: FrameState<Value>) {
        if self.frames.len() <= fi || self.frames[fi].base != base {
            return;
        }
        for (r, v) in state.regs {
            if let Some(slot) = self.regs.get_mut(base + r as usize) {
                *slot = v;
            }
        }
        if let Some(exit) = state.exit {
            let f = &mut self.frames[fi];
            f.pc = exit.pc as usize;
            f.acc = exit.acc;
            self.jit.stats.deopts += 1;
        }
    }

    /// Every compiled frame's state, written into its interpreter frame:
    /// what a debugger attaching does before it looks at the frames (ADR
    /// 0019 J9). Compiled code is not entered again while it is attached,
    /// and leaves at its next call out.
    pub(crate) fn jit_materialize_all(&mut self) {
        for i in 0..self.jit.frames.len() {
            let jf = &mut self.jit.frames[i];
            if jf.materialized {
                continue;
            }
            jf.materialized = true;
            let (fi, base, state) = (jf.fi, jf.base, jf.handle.state());
            self.jit_write_state(fi, base, state);
        }
    }

    /// Choose what machine code does itself in blocks compiled from now
    /// on ([`DIRECT_CALLS`], [`DIRECT_ENVS`], [`DIRECT_GLOBALS`]; overrides
    /// `AGNIJS_JIT_CALLS`). Set it before any block is compiled: every
    /// block the VM's callee table holds is compiled with one layout.
    pub fn set_jit_direct(&mut self, what: u32) {
        self.jit.direct = what;
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
        let state = tier.state.get();
        if state != Tier::COLD && state != Tier::CALLED {
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
        // Judged worth compiling, warming up: on its loops, or (a block
        // without one) on calls of it.
        if back_edge != (state == Tier::COLD) {
            return true;
        }
        let h = seen.saturating_add(1);
        let waiting = h <= hot_for(&fr.code);
        if waiting {
            tier.hotness.set(h);
        }
        waiting
    }

    /// Whether the interpreter may yet hand `code` to compiled code: the
    /// JIT is on and the block has not been left to the interpreter for
    /// good (or to compiled callers alone).
    #[inline]
    pub(crate) fn jit_live(&self, code: &CodeBlock) -> bool {
        self.jit.mode != JitMode::Off
            && code.feedback_if(self.feedback_owner).is_none_or(|f| {
                !matches!(
                    f.tier.state.get(),
                    Tier::NEVER | Tier::DIRECT | Tier::DEMAND
                )
            })
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
                Some(f) => matches!(
                    f.tier.state.get(),
                    Tier::NEVER | Tier::DIRECT | Tier::DEMAND
                ),
                None => true,
            };
            if never {
                return idle;
            }
            let code = self.frames[fi].code.clone();
            let pc = self.frames[fi].pc;
            let base = self.frames[fi].base;
            let Some(compiled) = self.jit_code(&code, base) else {
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
            if self.jit.frames.is_empty() {
                // No compiled code is on the native stack (every run of it
                // has a frame on the list): what it may nest over is
                // counted from here, and nothing the callee table gave up
                // is in use. It never nests past the host's stack budget
                // (`Engine::set_stack_budget`): with the budget used up,
                // every call is made through the runtime.
                self.jit.stack_floor = stack_position()
                    .saturating_sub(JIT_STACK)
                    .max(agnijs_js_parser::stack::limit())
                    .max(1);
                self.jit.retired.clear();
            }
            let judged = (self.jit.mode == JitMode::On).then(|| {
                (
                    code.clone(),
                    self.jit.stats.calls_out,
                    self.jit.stats.interpreted,
                )
            });
            let out = compiled.enter(&mut JitRt { vm: self, code }, pc as u32, acc);
            let work = self.interrupt_counter.wrapping_sub(self.jit.run_start);
            self.jit.stats.back_edges += u64::from(work);
            if let Some((code, calls_out, interpreted)) = judged {
                let s = &self.jit.stats;
                let cost = (s.calls_out - calls_out) * CALL_OUT_COST
                    + (s.interpreted - interpreted) * INTERPRETED_COST;
                self.jit_judge_run(&code, work, cost.min(u64::from(u32::MAX)) as u32);
            }
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

    /// Mode `On`'s judgement of a block by the runs the tier loop starts
    /// of it. Entering compiled code from the interpreter costs more than
    /// interpreting a few instructions, a call out costs more than the
    /// interpreter's dispatch of the same instruction, and a callee run in
    /// the interpreter from a call out costs more than a call does there.
    /// What pays for them is the work compiled code does without leaving:
    /// the loop back-edges it takes and the calls it makes directly
    /// ([`JitStats::back_edges`]). A block whose runs cost more than their
    /// work is worth ([`WORK_GAIN`] each) is not entered from the
    /// interpreter any more: a small function called from interpreted
    /// code, a loop that seldom turns, code that mostly calls out. Its
    /// code is kept for compiled callers, which call it in machine code
    /// without the entry (`Tier::DIRECT`). Judged per block every
    /// [`RUNS_JUDGED`] runs, whatever other blocks ran between, and at once
    /// when a single run has cost that much.
    fn jit_judge_run(&mut self, code: &CodeBlock, work: u32, cost: u32) {
        let Some(f) = code.feedback_if(self.feedback_owner) else {
            return;
        };
        let tier = &f.tier;
        let runs = tier.runs.get() + 1;
        let work = tier.work.get().saturating_add(work);
        let cost = tier
            .cost
            .get()
            .saturating_add(cost)
            .saturating_add(ENTRY_COST);
        if runs < RUNS_JUDGED && cost < RUNS_JUDGED * ENTRY_COST * 16 {
            tier.runs.set(runs);
            tier.work.set(work);
            tier.cost.set(cost);
            return;
        }
        tier.runs.set(0);
        tier.work.set(0);
        tier.cost.set(0);
        if u64::from(work) * u64::from(WORK_GAIN) < u64::from(cost)
            && tier.state.get() == Tier::COMPILED
        {
            tier.state.set(Tier::DIRECT);
            self.jit.stats.abandoned += 1;
        }
    }

    /// Whether a block without a loop, hot in mode `On`, is worth warming
    /// up and compiling. Its compiled code pays only when machine code
    /// calls it (the call is then made without leaving compiled code);
    /// entered from the interpreter it would cost more than it saves, and
    /// compiling costs what interpreting thousands of instructions does.
    /// So it is, when the frame calling it is one of a block that has
    /// compiled code, or of the block itself (recursion: its calls of
    /// itself will be direct). Otherwise it waits for compiled code to
    /// call it (`Tier::DEMAND`, [`Vm::jit_demand`]). The top frame is the
    /// block's.
    fn jit_wanted(&self, code: &Rc<CodeBlock>) -> bool {
        let n = self.frames.len();
        if n < 2 || self.frames[n - 1].exit {
            return false;
        }
        let caller = &self.frames[n - 2].code;
        Rc::ptr_eq(caller, code)
            || caller
                .feedback_if(self.feedback_owner)
                .is_some_and(|f| matches!(f.tier.state.get(), Tier::COMPILED | Tier::DIRECT))
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
            // A callee run in the interpreter from a call out costs about
            // what leaving for it and coming back did.
            entries: self.jit.stats.entries + self.jit.stats.interpreted,
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

    /// What to speculate about when compiling `code` for the frame whose
    /// window starts at `base`: the registers its loops do inline
    /// arithmetic on ([`agnijs_js_jit::number_candidates`]) that hold a
    /// number now, or nothing yet (a local not assigned before the loop:
    /// dead at the entry, so unguarded there), minus the guesses that have
    /// failed before (`Tier::demoted`).
    fn jit_guess(&self, code: &CodeBlock, tier: &Tier, base: usize) -> Speculation {
        let mut numbers = agnijs_js_jit::number_candidates(code) & !tier.demoted.get();
        let mut rest = numbers;
        while rest != 0 {
            let r = rest.trailing_zeros();
            rest &= rest - 1;
            match self.regs.get(base + r as usize) {
                Some(Value::Number(_)) | Some(Value::Undefined) => {}
                _ => numbers &= !(1u64 << r),
            }
        }
        Speculation { numbers }
    }

    /// The compiled code of `code`, compiling it if it has become hot, for
    /// the frame whose window starts at `base`. The caller holds the code
    /// it runs: a guard failing during the run replaces the block's code.
    fn jit_code(&mut self, code: &Rc<CodeBlock>, base: usize) -> Option<Rc<Compiled<Value>>> {
        // A script body may have none yet: it is created on first use.
        let tier = &code.feedback(self.feedback_owner)?.tier;
        match tier.state.get() {
            Tier::COMPILED => tier.code.borrow().clone()?.downcast().ok(),
            Tier::NEVER | Tier::DIRECT | Tier::DEMAND => None,
            _ => {
                let h = tier.hotness.get().saturating_add(1);
                tier.hotness.set(h);
                if h <= self.jit.threshold {
                    return None;
                }
                let layout = crate::object::jit_heap_layout();
                let calls = self.jit_calls(code);
                if self.jit.mode == JitMode::On {
                    // Judged when it first becomes hot, so that a block not
                    // worth compiling stops being counted, and again when
                    // it is hot enough to compile, by its feedback then.
                    let ready = h > hot_for(code);
                    let first = h == self.jit.threshold + 1;
                    if (ready || first)
                        && !agnijs_js_jit::profitable_for(code, layout.is_some(), calls.as_ref())
                    {
                        self.jit.stats.unprofitable += 1;
                        tier.state.set(Tier::NEVER);
                        return None;
                    }
                    if (first || ready) && !agnijs_js_jit::has_loop(code) {
                        if !self.jit_wanted(code) {
                            // Nothing that would call it directly calls
                            // it: it waits for that, uncounted.
                            tier.state.set(Tier::DEMAND);
                            return None;
                        }
                        // No back-edge to warm up on: calls of it count.
                        tier.state.set(Tier::CALLED);
                    }
                    if !ready {
                        return None;
                    }
                }
                self.jit_compile(code, base, layout.as_ref(), calls.as_ref())
            }
        }
    }

    /// Compile `code` for the frame whose window starts at `base`, and make
    /// the code the block's.
    fn jit_compile(
        &mut self,
        code: &Rc<CodeBlock>,
        base: usize,
        layout: Option<&agnijs_js_jit::HeapLayout>,
        calls: Option<&CallLayout>,
    ) -> Option<Rc<Compiled<Value>>> {
        let tier = &code.feedback(self.feedback_owner)?.tier;
        let spec = self.jit_guess(code, tier, base);
        match Compiled::<Value>::compile_calling(code, layout, calls, &spec) {
            Ok(c) => {
                let s = &mut self.jit.stats;
                s.compiled += 1;
                s.speculated += u32::from(spec.numbers != 0);
                s.code_bytes += c.code_size;
                s.compile_time += c.compile_time;
                tier.state.set(Tier::COMPILED);
                let c = Rc::new(c);
                *tier.code.borrow_mut() = Some(c.clone());
                Some(c)
            }
            Err(_) => {
                self.jit.stats.rejected += 1;
                tier.state.set(Tier::NEVER);
                None
            }
        }
    }

    /// Compiled code called a function whose block waits for that
    /// (`Tier::DEMAND`): the call is counted, and once there have been
    /// [`DEMAND_PER_INSTRUCTION`] per instruction the block is compiled, so
    /// that the calls after this one are made in machine code. The frame
    /// just pushed for the call is the top one.
    fn jit_demand(&mut self, code: &Rc<CodeBlock>) -> Option<Rc<Compiled<Value>>> {
        let tier = &code.feedback_if(self.feedback_owner)?.tier;
        if tier.state.get() != Tier::DEMAND || self.jit.mode != JitMode::On {
            return None;
        }
        let h = tier.hotness.get().saturating_add(1);
        tier.hotness.set(h);
        let wanted = (code.instrs.len() as u32).saturating_mul(DEMAND_PER_INSTRUCTION);
        if h - self.jit.threshold.min(h) <= wanted {
            return None;
        }
        let layout = crate::object::jit_heap_layout();
        let calls = self.jit_calls(code);
        if !agnijs_js_jit::profitable_for(code, layout.is_some(), calls.as_ref()) {
            self.jit.stats.unprofitable += 1;
            tier.state.set(Tier::NEVER);
            return None;
        }
        let base = self.frames.last()?.base;
        // The caller's loop paid for the calls so far; it is judged afresh
        // from here.
        self.jit.judge = Judge::default();
        self.jit_compile(code, base, layout.as_ref(), calls.as_ref())
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
                self.env_slot(fi, depth, slot)
            }
            Instr::StaEnv { depth, slot } => {
                *self.env_slot_mut(fi, depth, slot)? = acc;
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

/// The runtime compiled code calls back into: the VM, for one run of
/// compiled code ([`Compiled::enter`]). The frame an instruction belongs
/// to is always the top one: machine code runs the top frame only, and a
/// function it calls directly (TASK-101.3.3) has its frame on top while
/// it runs.
struct JitRt<'a> {
    vm: &'a mut Vm,
    /// The code block of the frame last served ([`JitRt::top`]): kept here
    /// so that the generic step can borrow it beside the VM, and replaced
    /// when a call out comes from a frame of another block (a function
    /// called directly, or its caller again).
    code: Rc<CodeBlock>,
}

/// What a call from compiled code costs the caller's loop, in call-outs:
/// leaving compiled code, the function-entry and return polls, and a new
/// run after the return that starts from the frame in memory. Set where a
/// loop of calls (Gable's signal-churn) stops losing to the interpreter;
/// the round trip alone measures about 3 call-outs.
const ENTRY_SHARE: u64 = 20;

/// Back-edges a block's runs must take before its loops are judged.
const JUDGE_AFTER: u32 = 1024;

/// Calls from compiled code, per instruction of the callee, before a
/// block waiting for them is compiled ([`Vm::jit_demand`]): fewer than
/// [`HOT_PER_INSTRUCTION`], since each call saved is a callee run in the
/// interpreter from a call out, which costs several plain calls.
const DEMAND_PER_INSTRUCTION: u32 = 50;

/// Runs of a block the tier loop starts between judgements of them
/// ([`Vm::jit_judge_run`]).
const RUNS_JUDGED: u32 = 256;

/// The judgement's units, about 4 ns each as measured on the benchmark
/// machine: what a unit of work saves over the interpreter (a loop
/// iteration run inline, or half a call made in machine code), and what a
/// run costs over it: the entry from the tier loop, a call out, a callee
/// run in the interpreter from a call out.
const WORK_GAIN: u32 = 5;
const ENTRY_COST: u32 = 15;
const CALL_OUT_COST: u64 = 2;
const INTERPRETED_COST: u64 = 12;

impl JitRt<'_> {
    /// The frame compiled code is calling out from, the top one, and its
    /// window's base; `self.code` is its block from here on.
    #[inline(always)]
    fn top(&mut self) -> (usize, usize) {
        let fi = self.vm.frames.len() - 1;
        let f = &self.vm.frames[fi];
        if !Rc::ptr_eq(&self.code, &f.code) {
            self.code = f.code.clone();
        }
        (fi, f.base)
    }

    /// Where execution goes after an instruction of frame `fi` run by the
    /// runtime.
    #[inline(never)]
    fn after(&mut self, fi: usize, flow: JsResult<Flow>) -> Resume<Value> {
        let vm = &mut *self.vm;
        match flow {
            Ok(Flow::Next) if vm.frames.len() == fi + 1 => {
                if vm.debug_hot {
                    // The interpreter's loop head handles the debugger
                    // before the next instruction.
                    return Resume::Exit(Exit::Interp);
                }
                if vm.heap.attention != 0 {
                    return self.attention(fi);
                }
                let f = &vm.frames[fi];
                Resume::Next {
                    pc: f.pc as u32,
                    acc: f.acc,
                }
            }
            Ok(Flow::Next) | Ok(Flow::Reload) if vm.frames.len() == fi + 2 => self.enter_callee(fi),
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

    /// The instruction just run in frame `fi` (a call, `new`, a getter or
    /// setter) pushed one frame over it: run the callee's compiled code
    /// from here, inside this call out, and continue the caller's compiled
    /// code when it returns (TASK-101.3.3). Nothing of the caller is
    /// outside its frame but what its safe point lists (it was written back
    /// for the call out), so the caller's machine frame below holds no
    /// value the collector cannot see and no state the interpreter frame
    /// lacks.
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
    /// A callee that has no compiled code (yet, or ever: a generator, a
    /// derived constructor) runs in the interpreter from here instead
    /// ([`JitRt::interpret_callee`]). A debugger, a fuel budget or the
    /// nesting bound leave the old way (`Exit::Frame`), before the poll, so
    /// the tier loop polls and counts the entry as it always has.
    #[inline(never)]
    fn enter_callee(&mut self, fi: usize) -> Resume<Value> {
        let callee = fi + 1;
        let vm = &mut *self.vm;
        if vm.debug_hot
            || vm.fuel_on
            || stack_position() < vm.jit.stack_floor.saturating_add(NEST_STACK)
        {
            return Resume::Exit(Exit::Frame);
        }
        let code = vm.frames[callee].code.clone();
        let pc = vm.frames[callee].pc;
        let compiled = match vm.jit_compiled(&code).or_else(|| vm.jit_demand(&code)) {
            Some(c) if c.is_entry(pc) => c,
            _ => return self.interpret_callee(fi),
        };
        let c0 = vm.interrupt_counter;
        let outer_start = vm.jit.run_start;
        let r = Self::run_callee(vm, callee, code, &compiled, pc);
        // The caller's run does not count the callee's back-edges or the
        // polls made for it.
        vm.jit.run_start = outer_start.wrapping_add(vm.interrupt_counter.wrapping_sub(c0));
        match r {
            Ok(Ok(())) => {
                // `after(Ok(Flow::Next))`, its common case here.
                if !vm.debug_hot && vm.heap.attention == 0 {
                    let f = &vm.frames[fi];
                    return Resume::Next {
                        pc: f.pc as u32,
                        acc: f.acc,
                    };
                }
                self.after(fi, Ok(Flow::Next))
            }
            Ok(Err(exit)) => Resume::Exit(exit),
            Err(fault) => {
                vm.jit.fault = Some(fault);
                Resume::Exit(Exit::Fault)
            }
        }
    }

    /// The frame pushed over frame `fi` has no compiled code to enter:
    /// run it in the interpreter from here, inside this call out, as a run
    /// loop of its own that ends when the frame returns, and continue the
    /// caller's compiled code then. Leaving compiled code instead
    /// (`Exit::Frame`) would unwind every machine frame on the native
    /// stack, each to be entered again from memory as the calls return: a
    /// compiled function that calls one the tier does not compile (a
    /// generator's body is never reached this way, but a derived
    /// constructor is, at every `new`) paid that at every call.
    ///
    /// The loop is the interpreter's own (`Vm::run_loop`): it polls at
    /// function entry, offers the frame and everything it calls to
    /// compiled code as it becomes hot, and services safe points. No Rust
    /// frame between the caller's machine code and the loop holds a heap
    /// value (the instruction's operands were in the caller's frame, which
    /// was written back), so a collection in the loop needs no retention
    /// bound beyond the caller's safe point, as in [`JitRt::enter_callee`].
    /// A fault that leaves the callee's frame comes back here with that
    /// frame popped, and goes on to the caller's handlers as a fault of
    /// the calling instruction.
    #[inline(never)]
    fn interpret_callee(&mut self, fi: usize) -> Resume<Value> {
        let vm = &mut *self.vm;
        let callee = fi + 1;
        // The loop ends when its exit frame returns; the frame is not one
        // (it was pushed by an instruction), so what `do_return` does for
        // such a frame is done here.
        let top = &mut vm.frames[callee];
        if top.exit {
            return Resume::Exit(Exit::Frame);
        }
        let discard = top.discard_result;
        top.exit = true;
        vm.jit.stats.interpreted += 1;
        let c0 = vm.interrupt_counter;
        let outer_start = vm.jit.run_start;
        let r = vm.run_loop();
        vm.jit.run_start = outer_start.wrapping_add(vm.interrupt_counter.wrapping_sub(c0));
        match r {
            Ok(RunOutcome::Return(v)) if vm.frames.len() == callee => {
                if !discard {
                    vm.frames[fi].acc = v;
                }
                // A call returned (J7).
                if let Err(fault) = vm.poll_interrupt() {
                    vm.jit.fault = Some(fault);
                    return Resume::Exit(Exit::Fault);
                }
                self.after(fi, Ok(Flow::Next))
            }
            Ok(outcome) => {
                // Not a plain function's frame after all: the run is over.
                vm.jit.done = Some(outcome);
                Resume::Exit(Exit::Done)
            }
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
        code: Rc<CodeBlock>,
        compiled: &Compiled<Value>,
        pc: usize,
    ) -> JsResult<Result<(), Exit>> {
        // Function entry (J7), then the safe point at the callee's first
        // instruction, as the tier loop makes them.
        vm.poll_interrupt()?;
        if vm.heap.attention != 0 {
            vm.attention_slow(callee)?;
        }
        vm.jit.stats.direct_calls += 1;
        vm.jit.run_start = vm.interrupt_counter;
        let acc = vm.frames[callee].acc;
        let out = compiled.enter(&mut JitRt { vm: &mut *vm, code }, pc as u32, acc);
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

    /// The safe point after an instruction of frame `fi`, frame written
    /// back, when the attention word asks for a collection (or the
    /// memory-limit check): what the interpreter's loop head does before
    /// the next instruction. Any collection runs here, under compiled code,
    /// which holds nothing of the heap across this call but what its safe
    /// point lists, and reloads the arena base after it.
    #[cold]
    #[inline(never)]
    fn attention(&mut self, fi: usize) -> Resume<Value> {
        let vm = &mut *self.vm;
        if vm.fuel_on {
            // A host call set a budget: the interpreter counts from the
            // next instruction.
            return Resume::Exit(Exit::Interp);
        }
        if let Err(fault) = vm.attention_slow(fi) {
            vm.jit.fault = Some(fault);
            return Resume::Exit(Exit::Fault);
        }
        if vm.frames.len() != fi + 1 {
            return Resume::Exit(Exit::Frame);
        }
        let f = &vm.frames[fi];
        Resume::Next {
            pc: f.pc as u32,
            acc: f.acc,
        }
    }

    /// [`Vm::unprofitable_runs`] at an interrupt poll on the back-edge from
    /// `pc` to `target`, judged by that loop's body.
    fn unprofitable_loop(&mut self, pc: u32, target: u32) -> bool {
        let body = u64::from(pc.saturating_sub(target)) + 1;
        self.vm.unprofitable_runs(&self.code, body, true)
    }

    #[inline(never)]
    fn generic(
        &mut self,
        fi: usize,
        base: usize,
        pc: usize,
        instr: Instr,
        acc: Value,
    ) -> Resume<Value> {
        let f = &mut self.vm.frames[fi];
        f.pc = pc + 1;
        f.acc = acc;
        let flow = self.vm.jit_generic(instr, fi, base, &self.code);
        self.after(fi, flow)
    }
}

impl Runtime<Value> for JitRt<'_> {
    fn slow(&mut self, pc: u32, acc: Value) -> Resume<Value> {
        self.vm.jit.stats.calls_out += 1;
        let (fi, base) = self.top();
        let pc = pc as usize;
        let instr = self.code.instrs[pc];

        if let Some(v) = self.vm.jit_fast(instr, fi, base, &self.code, acc) {
            if self.vm.heap.attention == 0 {
                return Resume::Next {
                    pc: pc as u32 + 1,
                    acc: v,
                };
            }
            let f = &mut self.vm.frames[fi];
            f.pc = pc + 1;
            f.acc = v;
            if self.vm.debug_hot {
                return Resume::Exit(Exit::Interp);
            }
            return self.attention(fi);
        }
        self.generic(fi, base, pc, instr, acc)
    }

    fn this_value(&mut self) -> Option<Value> {
        self.vm.frames.last().map(|f| f.this)
    }

    /// A cached global, a declarative environment slot or the callee: the hit
    /// in a small function; a miss, or a safe point due, by [`Runtime::slow`].
    fn fast(&mut self, pc: u32, acc: Value) -> Option<Value> {
        let vm = &mut *self.vm;
        if vm.heap.attention != 0 {
            return None;
        }
        let fi = vm.frames.len() - 1;
        let hit = match vm.frames[fi].code.instrs[pc as usize] {
            Instr::LdaGlobal(name) => vm.load_global_cached(&vm.frames[fi].code, name),
            Instr::LdaEnv { depth, slot } => vm.env_slot(fi, depth, slot),
            Instr::StaEnv { depth, slot } => vm.env_slot_mut(fi, depth, slot).map(|s| {
                *s = acc;
                acc
            }),
            Instr::LdaCallee => Some(vm.frames[fi].callee.map_or(Value::Undefined, Value::Object)),
            _ => None,
        };
        vm.jit.stats.calls_out += 1;
        hit
    }

    /// An `LdaGlobal` its site did not serve: the cached read, and the
    /// binding's address for the site, so that machine code reads it
    /// itself until the heap's global stamp moves.
    fn global(&mut self, pc: u32, _acc: Value, site: Option<&GlobalSite>) -> Option<Value> {
        let vm = &mut *self.vm;
        if vm.heap.attention != 0 {
            return None;
        }
        let fi = vm.frames.len() - 1;
        let Instr::LdaGlobal(name) = vm.frames[fi].code.instrs[pc as usize] else {
            return None;
        };
        vm.jit.stats.calls_out += 1;
        let (stamp, realm) = (vm.heap.global_stamp, vm.current_realm.0);
        let code = &vm.frames[fi].code;
        if let Some((v, at)) = vm.global_slot_cached(code, name) {
            if let Some(site) = site {
                site.fill(stamp, realm, at);
            }
            return Some(v);
        }
        let (v, at) = vm.global_lexical_cached(code, name)?;
        if let Some(site) = site {
            site.fill(stamp, realm, at);
        }
        Some(v)
    }

    fn global_stamp(&mut self) -> *const u64 {
        &self.vm.heap.global_stamp
    }

    /// `Call`, the dispatch loop's way (`call_from_frame`), without the
    /// general decode; a compiled callee's frame runs from here, and when
    /// it can be called directly it goes into the callee table, so that
    /// machine code makes the next call of it itself.
    fn call(&mut self, pc: u32, acc: Value) -> Resume<Value> {
        let (fi, base) = self.top();
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
        let f = &mut vm.frames[fi];
        f.pc = pc + 1;
        f.acc = acc;
        let fv = vm.regs[base + callee as usize];
        let t = vm.regs[base + this as usize];
        let a = Args::Regs {
            start: base + args as usize,
            len: argc as usize,
        };
        match vm.call_from_frame(fv, t, a) {
            Ok(Flow::Reload) if vm.frames.len() == fi + 2 => {
                vm.jit_add_callee(fv);
                self.enter_callee(fi)
            }
            flow => self.after(fi, flow),
        }
    }

    /// A function machine code called directly returned `acc` to the call
    /// at `pc`, and machine code left the rest of the return to the
    /// runtime: the pop (`do_return`) unless it popped the frame itself,
    /// then the poll after a call returns (J7) and the safe point.
    fn returned(&mut self, pc: u32, acc: Value, popped: bool) -> Resume<Value> {
        let _ = pc;
        let vm = &mut *self.vm;
        vm.jit.stats.calls_out += 1;
        let Some(top) = vm.frames.last_mut() else {
            return Resume::Exit(Exit::Interp);
        };
        // The callee's frame, for `do_return` to take the value from, or
        // (popped) the caller's, where `do_return` would have put it.
        top.acc = acc;
        if !popped {
            if let Flow::Done(outcome) = vm.do_return() {
                vm.jit.done = Some(outcome);
                return Resume::Exit(Exit::Done);
            }
        }
        if let Err(fault) = vm.poll_interrupt() {
            vm.jit.fault = Some(fault);
            return Resume::Exit(Exit::Fault);
        }
        let fi = vm.frames.len() - 1;
        self.after(fi, Ok(Flow::Next))
    }

    fn call_view(&mut self) -> CallView {
        let vm = &mut *self.vm;
        CallView {
            frames: &mut vm.frames as *mut Vec<Frame> as *mut u8,
            registers: &mut vm.regs as *mut Vec<Value> as *mut u8,
            realm: &mut vm.current_realm as *mut agnijs_engine_api::RealmId as *mut u8,
            callees: vm.jit.callees.base(),
        }
    }

    fn shape_epoch(&self) -> u64 {
        self.vm.heap.shape_epoch
    }

    fn stack_floor(&self) -> usize {
        self.vm.jit.stack_floor
    }

    fn direct_calls(&mut self, n: u64) {
        // (Each made the function-entry poll, and the poll after its
        // return, on the counter the run's work is counted by: a call made
        // in machine code is work done without leaving, like a back-edge.)
        self.vm.jit.stats.direct_calls += n;
    }

    fn heap(&mut self) -> HeapView {
        HeapView {
            objects: self.vm.heap.objects.base_ptr(),
            proto_epoch: self.vm.heap.proto_epoch,
            envs: self.vm.heap.envs.base_ptr(),
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
        let (fi, _) = self.top();
        let f = &mut self.vm.frames[fi];
        f.pc = pc as usize + 1;
        f.acc = acc;
        // Compiled code advanced the counter; the host poll is due when it
        // reached a multiple of 0x1000, else only the safe point is (GC
        // attention under stress), which `after` services.
        let ask = if self.vm.interrupt_counter & 0xfff == 0 {
            let asked = self.vm.ask_host_interrupt();
            if asked.is_ok() && self.unprofitable_loop(pc, target) {
                // Continue at the loop head in the interpreter, for good.
                self.vm.frames[fi].pc = target as usize;
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
            self.vm.frames[fi].pc = target as usize;
            Flow::Next
        });
        self.after(fi, flow)
    }

    fn safe_point_due(&self) -> bool {
        self.vm.heap.attention != 0
    }

    fn registers(&mut self) -> &mut [Value] {
        let base = self.vm.frames.last().map_or(0, |f| f.base);
        &mut self.vm.regs[base..]
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

    /// Only a frame [`Compiled::enter`] runs has a handle, and it is the
    /// top frame when it is entered.
    fn frame_entered(&mut self, frame: FrameHandle<Value>) {
        let fi = self.vm.frames.len() - 1;
        let base = self.vm.frames[fi].base;
        self.vm.jit.frames.push(JitFrame {
            handle: frame,
            fi,
            base,
            materialized: false,
        });
    }

    fn frame_left(&mut self) {
        let left = self.vm.jit.frames.pop();
        debug_assert!(left.is_some(), "compiled frames left out of order");
    }

    /// The state of the frame whose machine code is leaving, or continuing
    /// somewhere it did not expect. A frame that holds something outside
    /// the register file has a handle, and is the innermost such frame:
    /// the functions machine code called directly above it hold nothing
    /// (their states are empty), and every frame entered above it has
    /// left.
    fn materialize(&mut self, state: FrameState<Value>) {
        if state.regs.is_empty() && state.exit.is_none() {
            return;
        }
        let Some(top) = self.vm.jit.frames.last_mut() else {
            return;
        };
        if top.materialized && state.exit.is_none() {
            // Written by `jit_materialize_all` already, possibly changed
            // by the debugger since.
            return;
        }
        top.materialized = true;
        let (fi, base) = (top.fi, top.base);
        if let Some(exit) = &state.exit {
            if exit.reason == DeoptReason::Guard {
                if let Some(code) = self.vm.frames.get(fi).map(|f| f.code.clone()) {
                    self.vm.jit_demote(&code, exit.guard);
                }
            }
        }
        self.vm.jit_write_state(fi, base, state);
    }
}

/// A block whose guesses failed this often stops guessing.
const MAX_DEOPTS: u32 = 4;

impl Vm {
    /// A guess about register `guard` (`None`: any) of `code` failed: the
    /// block's code is dropped and it is compiled again, without that
    /// guess, the next time it is offered (its hotness is kept). The run
    /// that is in the old code holds it until it returns.
    fn jit_demote(&mut self, code: &CodeBlock, guard: Option<u16>) {
        let Some(f) = code.feedback_if(self.feedback_owner) else {
            return;
        };
        let tier = &f.tier;
        let n = tier.deopts.get().saturating_add(1);
        tier.deopts.set(n);
        let mut demoted = tier.demoted.get();
        match guard {
            Some(r) if r < 64 && n < MAX_DEOPTS => demoted |= 1u64 << r,
            _ => demoted = u64::MAX,
        }
        tier.demoted.set(demoted);
        if tier.state.get() == Tier::COMPILED {
            tier.state.set(Tier::COLD);
            *tier.code.borrow_mut() = None;
        }
    }

    /// The compiled code `code` has now, if any.
    fn jit_compiled(&self, code: &CodeBlock) -> Option<Rc<Compiled<Value>>> {
        match code.feedback_if(self.feedback_owner) {
            Some(f) if matches!(f.tier.state.get(), Tier::COMPILED | Tier::DIRECT) => f
                .tier
                .code
                .borrow()
                .clone()
                .and_then(|c| c.downcast::<Compiled<Value>>().ok()),
            _ => None,
        }
    }

    /// Compiled code's `Call` has just called `f` through the runtime,
    /// which pushed its frame (the top one): when `f` is a plain function
    /// whose compiled code machine code may call directly, put it in the
    /// callee table, so that machine code makes the next call of it
    /// itself ([`CalleeTable::fill`]).
    ///
    /// The frame image is a `Frame` as `push_function_frame` builds one for
    /// a plain call of a function of this block in this realm; machine
    /// code sets `base`, `this`, `argc`, the callee's id, `env`,
    /// `base_env` and `module` per call (`CallLayout`). `this` here is
    /// what a null or undefined `this` becomes for a function that takes
    /// the global object for it. A block that keeps its arguments (an
    /// arguments object, rest parameters) is left to the runtime, which
    /// copies them.
    fn jit_add_callee(&mut self, f: Value) {
        if self.jit.direct & DIRECT_CALLS == 0 {
            return;
        }
        let Value::Object(fo) = f else { return };
        let Some(fd) = self.heap.obj(fo).function() else {
            // `call` or `apply`: the frame is another function's.
            return;
        };
        let Some(word) = agnijs_js_jit::rc_word(&fd.code) else {
            return;
        };
        if self.jit.callees.has(word, fd.realm.0) || !self.jit.callees.takes(word) {
            // Machine code had another reason to call through the runtime,
            // or the table has no room for this function.
            return;
        }
        let (code, realm, this_mode) = (fd.code.clone(), fd.realm, fd.this_mode);
        if fd.is_class_constructor
            || code.function_kind != FunctionKind::Normal
            || code.keeps_arguments
            || self.frames.last().is_none_or(|top| top.callee != Some(fo))
        {
            return;
        }
        let Some(fb) = code.feedback_if(self.feedback_owner) else {
            return;
        };
        let Some(compiled) = self.jit_compiled(&code) else {
            return;
        };
        if !compiled.direct() {
            return;
        }
        let this = match this_mode {
            ThisMode::Global => Value::Object(self.realm_of(realm).global),
            _ => Value::Undefined,
        };
        let template = Frame {
            code: code.clone(),
            pc: 0,
            base: 0,
            acc: Value::Undefined,
            env: None,
            base_env: None,
            env_depth: 0,
            this,
            callee: Some(fo),
            new_target: Value::Undefined,
            args: Vec::new(),
            argc: 0,
            realm,
            exit: false,
            discard_result: false,
            construct: ConstructKind::None,
            kind: FrameKind::Function,
            generator: None,
            module: None,
        };
        let old = self.jit.callees.fill(Callee {
            compiled: &compiled,
            code: word,
            template: Box::new(template),
            ic: &fb.ic,
            shape_epoch: &fb.shape_epoch,
            registers: u32::from(code.register_count),
            param_base: u32::from(code.param_base),
            param_regs: u32::from(code.param_regs),
            realm: realm.0,
        });
        // A frame of the callee this one replaced may still be running
        // below this call: its code is kept until no compiled code runs.
        self.jit.retired.extend(old);
    }
}

/// [`JitState::direct`]: machine code calls compiled functions itself.
pub const DIRECT_CALLS: u32 = 1;
/// [`JitState::direct`]: machine code reads and writes environment slots.
pub const DIRECT_ENVS: u32 = 2;
/// [`JitState::direct`]: machine code reads global bindings.
pub const DIRECT_GLOBALS: u32 = 4;

impl Vm {
    /// The call layout to compile `code` with: the VM's, less what
    /// [`JitState::direct`] turns off.
    fn jit_calls(&self, code: &Rc<CodeBlock>) -> Option<CallLayout> {
        let on = self.jit.direct;
        if on == 0 {
            return None;
        }
        let mut layout = jit_call_layout(code)?;
        layout.direct &= on & DIRECT_CALLS != 0;
        if on & DIRECT_ENVS == 0 {
            layout.env = None;
        }
        layout.globals &= on & DIRECT_GLOBALS != 0;
        Some(layout)
    }
}

/// What compiled code needs to push and pop this VM's frames itself
/// (TASK-101.3.3): `None`, and every call through the runtime, if anything
/// is not as compiled code reads and writes it. Computed once per process,
/// in safe code, from the VM's own types: offsets are `offset_of!` or
/// differences of addresses, never dereferenced. `code` is any block: a
/// function's data is probed on a real one.
fn jit_call_layout(code: &Rc<CodeBlock>) -> Option<CallLayout> {
    static LAYOUT: std::sync::OnceLock<Option<CallLayout>> = std::sync::OnceLock::new();
    *LAYOUT.get_or_init(|| compute_call_layout(code))
}

fn compute_call_layout(code: &Rc<CodeBlock>) -> Option<CallLayout> {
    use crate::object::{class_tag, Class, ConstructorKind, FunctionData, ObjectData};
    use crate::value::{EnvId, ObjId};
    use agnijs_engine_api::RealmId;
    use std::mem::{offset_of, size_of};
    fn addr<T>(r: &T) -> usize {
        r as *const T as usize
    }
    // A frame's and a function's environment are what an environment
    // record's parent is: one optional id, read one way.
    let _: fn(&Frame, &crate::env::DeclEnv) = |f, d| same(&f.env, &d.parent);
    // Fields compiled code copies from a function's data into a frame must
    // be of one type in both.
    fn same<T>(_: &T, _: &T) {}
    let u = |n: usize| u32::try_from(n).ok();
    let word = size_of::<usize>();

    // A reference to a code block is one word, the address of its count.
    agnijs_js_jit::rc_word(code)?;
    if size_of::<Rc<CodeBlock>>() != word
        || size_of::<Box<FunctionData>>() != word
        || size_of::<RealmId>() != 4
        || size_of::<ObjId>() != 4
        || size_of::<ThisMode>() != 1
        || size_of::<usize>() != 8
    {
        return None;
    }
    let env_size = size_of::<Option<EnvId>>();
    let module_size = size_of::<Option<ObjId>>();
    if !matches!(env_size, 4 | 8) || !matches!(module_size, 4 | 8) {
        return None;
    }
    // A function object's data: the box in the class, at the same place
    // whatever the function.
    let class = Class::Function(Box::new(FunctionData {
        code: code.clone(),
        env: None,
        realm: RealmId(0),
        home_object: None,
        constructor_kind: ConstructorKind::None,
        this_mode: ThisMode::Global,
        is_class_constructor: false,
        class_data: None,
        module: None,
    }));
    let function_data = match &class {
        Class::Function(b) => addr(b) - addr(&class),
        _ => return None,
    };
    // The callee of a frame: the id inside "some function".
    let some = Some(ObjId(7));
    let callee = addr(some.as_ref()?) - addr(&some);
    let _: fn(&FunctionData, &Frame) = |fd, top| {
        same(&fd.env, &top.env);
        same(&fd.env, &top.base_env);
        same(&fd.module, &top.module);
        same(&fd.realm, &top.realm);
        same(&fd.code, &top.code);
    };
    Some(CallLayout {
        frames: agnijs_js_jit::vec_layout_of::<Frame>()?,
        registers: agnijs_js_jit::vec_layout(Value::Undefined)?,
        frame_size: u(size_of::<Frame>())?,
        max_frames: u(crate::vm::MAX_FRAMES)?,
        frame_code: u(offset_of!(Frame, code))?,
        frame_pc: u(offset_of!(Frame, pc))?,
        frame_base: u(offset_of!(Frame, base))?,
        frame_acc: u(offset_of!(Frame, acc))?,
        frame_this: u(offset_of!(Frame, this))?,
        frame_argc: u(offset_of!(Frame, argc))?,
        frame_callee: u(offset_of!(Frame, callee) + callee)?,
        frame_env: u(offset_of!(Frame, env))?,
        frame_base_env: u(offset_of!(Frame, base_env))?,
        env_size: u(env_size)?,
        frame_module: u(offset_of!(Frame, module))?,
        module_size: u(module_size)?,
        frame_realm: u(offset_of!(Frame, realm))?,
        class_function: class_tag::FUNCTION,
        function_data: u(ObjectData::OFFSET_CLASS + function_data)?,
        fn_code: u(offset_of!(FunctionData, code))?,
        fn_env: u(offset_of!(FunctionData, env))?,
        fn_module: u(offset_of!(FunctionData, module))?,
        fn_realm: u(offset_of!(FunctionData, realm))?,
        fn_this_mode: u(offset_of!(FunctionData, this_mode))?,
        this_mode_global: ThisMode::Global as u8,
        direct: true,
        env: compute_env_layout(),
        globals: true,
    })
}

/// Where compiled code finds environment records: `None`, and every
/// environment access through the runtime, if anything is not as compiled
/// code reads it.
fn compute_env_layout() -> Option<EnvLayout> {
    use crate::env::{Environment, KIND_DECLARATIVE};
    use crate::value::EnvId;
    use std::mem::size_of;
    fn addr<T>(r: &T) -> usize {
        r as *const T as usize
    }
    let u = |n: usize| u32::try_from(n).ok();
    if size_of::<EnvId>() != 4 {
        return None;
    }
    // A record in its arena slot, with a parent.
    let probe = Some(Environment::synthetic(1, Some(EnvId(7)), false));
    let env = probe.as_ref()?;
    let Environment::Declarative(d) = env else {
        return None;
    };
    let id = addr(d.parent.as_ref()?) - addr(&d.parent);
    let (opt_tag, opt_some) = agnijs_js_jit::option_tag(&None::<EnvId>, &d.parent, id)?;
    let slots = agnijs_js_jit::vec_layout(Value::Undefined)?;
    let slots_at = addr(&d.slots) - addr(env);
    Some(EnvLayout {
        stride: u(size_of::<Option<Environment>>())?,
        offset: u(addr(env) - addr(&probe))?,
        // `#[repr(u8)]`: the kind is the record's first byte.
        kind: 0,
        declarative: KIND_DECLARATIVE,
        slots_ptr: u(slots_at + slots.ptr as usize)?,
        slots_len: u(slots_at + slots.len as usize)?,
        parent: u(addr(&d.parent) - addr(env))?,
        opt_tag,
        opt_some,
        opt_id: u(id)?,
    })
}
