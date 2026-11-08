//! Generators, async functions and async generators.
//!
//! A suspended function is a [`SavedFrame`]: the frame plus a copy of its
//! registers. Resuming pushes it back as an exit frame and runs a nested
//! interpreter loop until the next suspension or completion.

use super::{ConstructKind, Frame, RunOutcome};
use crate::builtins::promise::{self, Capability};
use crate::gc::Tracer;
use crate::object::{CallArgs, Class, ObjectData};
use crate::realm::Intrinsic;
use crate::value::{ObjId, PropKey, Value};
use crate::vm::{Fault, JsResult, Vm};
use gable_js_compiler::bytecode::{RESUME_NEXT, RESUME_RETURN, RESUME_THROW};
use std::collections::VecDeque;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GenKind {
    /// `function*`
    Sync,
    /// `async function*`
    Async,
    /// `async function` (the state object is internal).
    AsyncFn,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GenState {
    SuspendedStart,
    SuspendedYield,
    Executing,
    /// Async generator: awaiting the operand of a `return` request.
    AwaitingReturn,
    Completed,
}

pub struct SavedFrame {
    pub frame: Frame,
    pub regs: Vec<Value>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResumeMode {
    Next,
    Throw,
    Return,
}

impl ResumeMode {
    fn code(self) -> i32 {
        match self {
            ResumeMode::Next => RESUME_NEXT,
            ResumeMode::Throw => RESUME_THROW,
            ResumeMode::Return => RESUME_RETURN,
        }
    }
}

/// An AsyncGeneratorRequest.
pub struct AsyncRequest {
    pub mode: ResumeMode,
    pub value: Value,
    pub capability: Capability,
}

pub struct GeneratorData {
    pub kind: GenKind,
    pub state: GenState,
    pub saved: Option<SavedFrame>,
    /// Register that receives the resume mode at a `Yield`; `None` after
    /// an `Await`.
    pub resume_reg: Option<u16>,
    pub queue: VecDeque<AsyncRequest>,
    /// Async functions: the result promise.
    pub promise: Option<Capability>,
}

impl GeneratorData {
    pub fn trace(&self, t: &mut Tracer) {
        if let Some(s) = &self.saved {
            s.frame.trace(t);
            t.values(&s.regs);
        }
        for r in &self.queue {
            t.value(r.value);
            r.capability.trace(t);
        }
        if let Some(c) = &self.promise {
            c.trace(t);
        }
    }
}

/// Called whenever a generator-driven frame leaves the stack for good.
pub fn mark_completed(vm: &mut Vm, gen: ObjId) {
    if let Class::Generator(g) = &mut vm.heap.obj_mut(gen).class {
        g.state = GenState::Completed;
        g.saved = None;
    }
}

fn gen_data(vm: &mut Vm, gen: ObjId) -> &mut GeneratorData {
    match &mut vm.heap.obj_mut(gen).class {
        Class::Generator(g) => g,
        _ => panic!("not a generator object"),
    }
}

impl Vm {
    /// Save the frame `fi` (the top frame) into its generator object and
    /// pop it.
    pub(crate) fn suspend_frame(&mut self, fi: usize, resume_reg: Option<u16>) {
        debug_assert_eq!(fi, self.frames.len() - 1);
        let base = self.frames[fi].base;
        let regs = self.regs[base..].to_vec();
        let frame = self.pop_frame();
        let gen = frame
            .generator
            .expect("suspending a frame without a generator");
        let g = gen_data(self, gen);
        g.saved = Some(SavedFrame { frame, regs });
        g.resume_reg = resume_reg;
        g.state = if resume_reg.is_some() {
            GenState::SuspendedYield
        } else {
            GenState::Executing
        };
    }

    /// `InitialYield`: create the generator object and park the frame.
    pub(crate) fn generator_initial_yield(&mut self, fi: usize) -> JsResult<ObjId> {
        let callee = self.frames[fi].callee.expect("generator without a callee");
        let is_async = self.frames[fi].code.function_kind.is_async();
        let default = if is_async {
            Intrinsic::AsyncGeneratorPrototype
        } else {
            Intrinsic::GeneratorPrototype
        };
        let proto = self.get_prototype_from_constructor(Value::Object(callee), default)?;
        let data = GeneratorData {
            kind: if is_async {
                GenKind::Async
            } else {
                GenKind::Sync
            },
            state: GenState::SuspendedStart,
            saved: None,
            resume_reg: None,
            queue: VecDeque::new(),
            promise: None,
        };
        let gen = self.alloc(ObjectData::new(
            Some(proto),
            Class::Generator(Box::new(data)),
        ));
        self.frames[fi].generator = Some(gen);
        // The frame is popped by the caller's Return path: emulate a
        // suspension here instead.
        let base = self.frames[fi].base;
        let regs = self.regs[base..].to_vec();
        let mut frame = self.pop_frame();
        frame.exit = true;
        let g = gen_data(self, gen);
        g.saved = Some(SavedFrame { frame, regs });
        g.resume_reg = None;
        Ok(gen)
    }

    /// Push the saved frame back and run it with the given resumption.
    /// Returns how the run ended; faults are those of the body.
    fn resume_saved(&mut self, gen: ObjId, mode: ResumeMode, value: Value) -> JsResult<RunOutcome> {
        let (saved, resume_reg) = {
            let g = gen_data(self, gen);
            g.state = GenState::Executing;
            (
                g.saved
                    .take()
                    .expect("resuming a generator with no saved frame"),
                g.resume_reg,
            )
        };
        self.in_native(|vm| {
            let mut frame = saved.frame;
            frame.exit = true;
            frame.generator = Some(gen);
            vm.push_frame(frame)?;
            let base = vm.frames.last().unwrap().base;
            let n = saved.regs.len();
            vm.regs[base..base + n].copy_from_slice(&saved.regs);
            let fi = vm.frames.len() - 1;
            vm.frames[fi].acc = value;
            match resume_reg {
                Some(r) => {
                    vm.regs[base + r as usize] = Value::Number(mode.code() as f64);
                }
                None => {
                    // After an `Await`: a rejection is thrown at the await.
                    if mode == ResumeMode::Throw {
                        if let Err(f) = vm.handle_fault(Fault::Throw(value)) {
                            return Err(f);
                        }
                    }
                }
            }
            vm.run()
        })
    }

    // ---- sync generators --------------------------------------------------------

    /// GeneratorResume / GeneratorResumeAbrupt. Returns an iterator result.
    pub fn generator_resume(
        &mut self,
        gen: Value,
        mode: ResumeMode,
        value: Value,
        method: &str,
    ) -> JsResult<Value> {
        let Value::Object(go) = gen else {
            return Err(
                self.throw_type(&format!("{method} method called on incompatible receiver"))
            );
        };
        let state = match &self.heap.obj(go).class {
            Class::Generator(g) if g.kind == GenKind::Sync => g.state,
            _ => {
                return Err(
                    self.throw_type(&format!("{method} method called on incompatible receiver"))
                )
            }
        };
        match state {
            GenState::Executing => return Err(self.throw_type("Generator is already running")),
            GenState::Completed => {
                return match mode {
                    ResumeMode::Next => Ok(self.create_iter_result(Value::Undefined, true)),
                    ResumeMode::Return => Ok(self.create_iter_result(value, true)),
                    ResumeMode::Throw => Err(Fault::Throw(value)),
                };
            }
            GenState::SuspendedStart if mode != ResumeMode::Next => {
                mark_completed(self, go);
                return match mode {
                    ResumeMode::Return => Ok(self.create_iter_result(value, true)),
                    _ => Err(Fault::Throw(value)),
                };
            }
            _ => {}
        }
        match self.resume_saved(go, mode, value) {
            Ok(RunOutcome::Return(v)) => {
                mark_completed(self, go);
                Ok(self.create_iter_result(v, true))
            }
            Ok(RunOutcome::Yield(v)) => Ok(self.create_iter_result(v, false)),
            Ok(RunOutcome::YieldRaw(v)) => Ok(v),
            Ok(other) => panic!("sync generator run ended with {other:?}"),
            Err(f) => {
                mark_completed(self, go);
                Err(f)
            }
        }
    }

    pub fn create_iter_result(&mut self, value: Value, done: bool) -> Value {
        let o = self.new_object();
        let vk = PropKey::Str(self.names.value);
        self.heap.add_own_property(
            o,
            vk,
            crate::value::Property::data(value, crate::value::Attrs::ALL),
        );
        let dk = PropKey::Str(self.names.done);
        self.heap.add_own_property(
            o,
            dk,
            crate::value::Property::data(Value::Bool(done), crate::value::Attrs::ALL),
        );
        Value::Object(o)
    }

    // ---- async generators -------------------------------------------------------------

    /// AsyncGeneratorEnqueue + resume/drain. Returns the request promise.
    pub fn async_generator_enqueue(
        &mut self,
        gen: Value,
        mode: ResumeMode,
        value: Value,
    ) -> JsResult<Value> {
        let cap = promise::new_promise_capability_intrinsic(self);
        let go = match gen {
            Value::Object(go) if matches!(&self.heap.obj(go).class, Class::Generator(g) if g.kind == GenKind::Async) => {
                go
            }
            _ => {
                let err = self.make_error(
                    crate::vm::ErrorType::TypeError,
                    "AsyncGenerator method called on incompatible receiver",
                );
                promise::reject_capability(self, &cap, err)?;
                return Ok(Value::Object(cap.promise));
            }
        };
        let request = AsyncRequest {
            mode,
            value,
            capability: cap.clone(),
        };
        let state = {
            let g = gen_data(self, go);
            g.queue.push_back(request);
            g.state
        };
        if state != GenState::Executing && state != GenState::AwaitingReturn {
            self.async_generator_resume_next(go)?;
        }
        Ok(Value::Object(cap.promise))
    }

    /// AsyncGeneratorResumeNext / AsyncGeneratorDrainQueue.
    fn async_generator_resume_next(&mut self, go: ObjId) -> JsResult<()> {
        loop {
            let (state, head) = {
                let g = gen_data(self, go);
                match g.queue.front() {
                    None => return Ok(()),
                    Some(r) => (g.state, (r.mode, r.value)),
                }
            };
            debug_assert!(state != GenState::Executing);
            if state == GenState::AwaitingReturn {
                return Ok(());
            }
            let (mode, value) = head;
            if mode != ResumeMode::Next {
                if state == GenState::SuspendedStart {
                    mark_completed(self, go);
                }
                let state = gen_data(self, go).state;
                if state == GenState::Completed {
                    if mode == ResumeMode::Return {
                        // AsyncGeneratorAwaitReturn
                        gen_data(self, go).state = GenState::AwaitingReturn;
                        let p = match promise::promise_resolve_intrinsic(self, value) {
                            Ok(p) => p,
                            Err(Fault::Throw(e)) => {
                                gen_data(self, go).state = GenState::Completed;
                                self.async_generator_complete_step(go, Err(e), true)?;
                                continue;
                            }
                            Err(f) => return Err(f),
                        };
                        let on_ok = self.new_native_closure(
                            "",
                            1,
                            async_gen_return_fulfilled,
                            vec![Value::Object(go)],
                        );
                        let on_err = self.new_native_closure(
                            "",
                            1,
                            async_gen_return_rejected,
                            vec![Value::Object(go)],
                        );
                        promise::perform_then(
                            self,
                            p,
                            Value::Object(on_ok),
                            Value::Object(on_err),
                            None,
                        )?;
                        return Ok(());
                    } else {
                        self.async_generator_complete_step(go, Err(value), true)?;
                        continue;
                    }
                }
            } else if state == GenState::Completed {
                self.async_generator_complete_step(go, Ok(Value::Undefined), true)?;
                continue;
            }
            // Resume the body.
            let outcome = self.resume_saved(go, mode, value);
            match outcome {
                Ok(RunOutcome::Yield(v)) => {
                    self.async_generator_complete_step(go, Ok(v), false)?;
                    // The frame is saved with state SuspendedYield; keep
                    // draining if more requests are waiting.
                }
                Ok(RunOutcome::Await(v)) => {
                    self.async_await(go, v)?;
                    return Ok(());
                }
                Ok(RunOutcome::Return(v)) => {
                    mark_completed(self, go);
                    self.async_generator_complete_step(go, Ok(v), true)?;
                }
                Ok(RunOutcome::InitialYield | RunOutcome::YieldRaw(_)) => unreachable!(),
                Err(Fault::Throw(e)) => {
                    mark_completed(self, go);
                    self.async_generator_complete_step(go, Err(e), true)?;
                }
                Err(f) => return Err(f),
            }
        }
    }

    /// AsyncGeneratorCompleteStep: settle the head request.
    fn async_generator_complete_step(
        &mut self,
        go: ObjId,
        completion: Result<Value, Value>,
        done: bool,
    ) -> JsResult<()> {
        let req = gen_data(self, go)
            .queue
            .pop_front()
            .expect("complete step with an empty queue");
        match completion {
            Ok(v) => {
                let r = self.create_iter_result(v, done);
                promise::resolve_capability(self, &req.capability, r)?;
            }
            Err(e) => {
                promise::reject_capability(self, &req.capability, e)?;
            }
        }
        Ok(())
    }

    /// Await(v) for the body of `go`: hook the resumption to the promise.
    pub(crate) fn async_await(&mut self, go: ObjId, v: Value) -> JsResult<()> {
        let p = match promise::promise_resolve_intrinsic(self, v) {
            Ok(p) => p,
            Err(Fault::Throw(e)) => {
                // Awaiting a value whose `constructor` getter throws: the
                // body sees the exception on resumption.
                return self.async_resume(go, ResumeMode::Throw, e);
            }
            Err(f) => return Err(f),
        };
        let on_ok = self.new_native_closure("", 1, async_fulfilled, vec![Value::Object(go)]);
        let on_err = self.new_native_closure("", 1, async_rejected, vec![Value::Object(go)]);
        promise::perform_then(self, p, Value::Object(on_ok), Value::Object(on_err), None)?;
        Ok(())
    }

    /// Resume an awaiting body (async function or async generator) after
    /// its promise settled.
    pub(crate) fn async_resume(
        &mut self,
        go: ObjId,
        mode: ResumeMode,
        value: Value,
    ) -> JsResult<()> {
        let kind = gen_data(self, go).kind;
        match kind {
            GenKind::AsyncFn => {
                let outcome = self.resume_saved(go, mode, value);
                self.async_function_settle(go, outcome)
            }
            GenKind::Async => {
                let outcome = self.resume_saved(go, mode, value);
                match outcome {
                    Ok(RunOutcome::Await(v)) => self.async_await(go, v),
                    Ok(RunOutcome::Yield(v)) => {
                        self.async_generator_complete_step(go, Ok(v), false)?;
                        self.async_generator_resume_next(go)
                    }
                    Ok(RunOutcome::Return(v)) => {
                        mark_completed(self, go);
                        self.async_generator_complete_step(go, Ok(v), true)?;
                        self.async_generator_resume_next(go)
                    }
                    Ok(RunOutcome::InitialYield | RunOutcome::YieldRaw(_)) => unreachable!(),
                    Err(Fault::Throw(e)) => {
                        mark_completed(self, go);
                        self.async_generator_complete_step(go, Err(e), true)?;
                        self.async_generator_resume_next(go)
                    }
                    Err(f) => Err(f),
                }
            }
            GenKind::Sync => unreachable!(),
        }
    }

    /// Finish one step of an async function body.
    fn async_function_settle(&mut self, go: ObjId, outcome: JsResult<RunOutcome>) -> JsResult<()> {
        let cap = gen_data(self, go)
            .promise
            .clone()
            .expect("async function without a promise");
        match outcome {
            Ok(RunOutcome::Await(v)) => self.async_await(go, v),
            Ok(RunOutcome::Return(v)) => {
                mark_completed(self, go);
                promise::resolve_capability(self, &cap, v)?;
                Ok(())
            }
            Ok(other) => panic!("async function run ended with {other:?}"),
            Err(Fault::Throw(e)) => {
                mark_completed(self, go);
                promise::reject_capability(self, &cap, e)?;
                Ok(())
            }
            Err(f) => Err(f),
        }
    }
}

/// Call an `async function`: run until the first await and return the
/// result promise.
pub fn call_async_function(
    vm: &mut Vm,
    fo: ObjId,
    this: Value,
    args: Vec<Value>,
) -> JsResult<Value> {
    let cap = promise::new_promise_capability_intrinsic(vm);
    let data = GeneratorData {
        kind: GenKind::AsyncFn,
        state: GenState::Executing,
        saved: None,
        resume_reg: None,
        queue: VecDeque::new(),
        promise: Some(cap.clone()),
    };
    let proto = vm.intrinsic(Intrinsic::ObjectPrototype);
    let go = vm.alloc(ObjectData::new(
        Some(proto),
        Class::Generator(Box::new(data)),
    ));
    let outcome = vm.in_native(|vm| {
        vm.push_function_frame(fo, this, args, Value::Undefined, ConstructKind::None, true)?;
        let fi = vm.frames.len() - 1;
        vm.frames[fi].generator = Some(go);
        vm.run()
    });
    vm.async_function_settle(go, outcome)?;
    Ok(Value::Object(cap.promise))
}

fn async_fulfilled(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let go = vm.native_slot(a.callee, 0).as_object().unwrap();
    vm.async_resume(go, ResumeMode::Next, a.arg(0))?;
    Ok(Value::Undefined)
}

fn async_rejected(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let go = vm.native_slot(a.callee, 0).as_object().unwrap();
    vm.async_resume(go, ResumeMode::Throw, a.arg(0))?;
    Ok(Value::Undefined)
}

fn async_gen_return_fulfilled(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let go = vm.native_slot(a.callee, 0).as_object().unwrap();
    gen_data(vm, go).state = GenState::Completed;
    vm.async_generator_complete_step(go, Ok(a.arg(0)), true)?;
    vm.async_generator_resume_next(go)?;
    Ok(Value::Undefined)
}

fn async_gen_return_rejected(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let go = vm.native_slot(a.callee, 0).as_object().unwrap();
    gen_data(vm, go).state = GenState::Completed;
    vm.async_generator_complete_step(go, Err(a.arg(0)), true)?;
    vm.async_generator_resume_next(go)?;
    Ok(Value::Undefined)
}
