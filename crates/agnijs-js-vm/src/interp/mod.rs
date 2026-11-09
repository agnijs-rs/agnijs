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

use crate::value::{EnvId, ObjId, Value};
use crate::vm::{Fault, JsResult, Vm};
use agnijs_engine_api::RealmId;
use agnijs_js_compiler::bytecode::CodeBlock;
use std::rc::Rc;

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
    pub fn run(&mut self) -> JsResult<RunOutcome> {
        debug_assert!(self.frames.last().map(|f| f.exit).unwrap_or(false));
        let mut fi = self.frames.len() - 1;
        let mut code = self.frames[fi].code.clone();
        let mut base = self.frames[fi].base;
        loop {
            if self.heap.stress
                || self.gc_requested
                || self.heap.allocated_since_gc >= self.heap.gc_threshold
                || self.debug_hot
            {
                if self.debug_hot {
                    self.debug_check(fi);
                }
                // The stress interval is counted inside maybe_collect.
                self.maybe_collect();
                if let Some(limit) = self.heap.memory_limit {
                    // Under a native frame (a job, a callback) the collector
                    // is deferred, so everything allocated since the last
                    // collection is still held: count it, or a loop inside a
                    // callback would grow past the limit unchecked.
                    if self.heap.live_bytes + self.heap.allocated_since_gc > limit {
                        return Err(Fault::OutOfMemory);
                    }
                }
            }
            let pc = self.frames[fi].pc;
            let instr = code.instrs[pc];
            self.frames[fi].pc = pc + 1;
            let flow = match self.step(instr, fi, base, &code) {
                Ok(f) => f,
                Err(fault) => match self.handle_fault(fault) {
                    Ok(()) => Flow::Reload,
                    Err(f) => return Err(f),
                },
            };
            match flow {
                Flow::Next => {}
                Flow::Reload => {
                    fi = self.frames.len() - 1;
                    code = self.frames[fi].code.clone();
                    base = self.frames[fi].base;
                }
                Flow::Done(outcome) => return Ok(outcome),
            }
        }
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
