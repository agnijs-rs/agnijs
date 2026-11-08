//! Promise objects, reaction jobs and the Promise constructor.

use crate::gc::Tracer;
use crate::object::{CallArgs, Class, ObjectData};
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, PropKey, Value};
use crate::vm::{ErrorType, Fault, Job, JsResult, Vm};
use gable_engine_api::RejectionEvent;
use std::cell::Cell;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PromiseStatus {
    Pending,
    Fulfilled,
    Rejected,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReactionKind {
    Fulfill,
    Reject,
}

/// A PromiseCapability record.
#[derive(Clone)]
pub struct Capability {
    pub promise: ObjId,
    pub resolve: Value,
    pub reject: Value,
}

impl Capability {
    pub fn trace(&self, t: &mut Tracer) {
        t.obj(self.promise);
        t.value(self.resolve);
        t.value(self.reject);
    }
}

/// A PromiseReaction record.
#[derive(Clone)]
pub struct Reaction {
    pub capability: Option<Capability>,
    pub kind: ReactionKind,
    /// The handler, or `Undefined` for the default behaviour.
    pub handler: Value,
}

impl Reaction {
    pub fn trace(&self, t: &mut Tracer) {
        if let Some(c) = &self.capability {
            c.trace(t);
        }
        t.value(self.handler);
    }
}

pub struct PromiseData {
    pub status: PromiseStatus,
    pub result: Value,
    pub fulfill_reactions: Vec<Reaction>,
    pub reject_reactions: Vec<Reaction>,
    pub is_handled: bool,
}

impl PromiseData {
    pub fn trace(&self, t: &mut Tracer) {
        t.value(self.result);
        for r in self
            .fulfill_reactions
            .iter()
            .chain(self.reject_reactions.iter())
        {
            r.trace(t);
        }
    }
}

fn promise_data(vm: &Vm, p: ObjId) -> Option<&PromiseData> {
    match &vm.heap.obj(p).class {
        Class::Promise(d) => Some(d),
        _ => None,
    }
}

fn promise_data_mut(vm: &mut Vm, p: ObjId) -> &mut PromiseData {
    match &mut vm.heap.obj_mut(p).class {
        Class::Promise(d) => d,
        _ => panic!("not a promise"),
    }
}

pub fn is_promise(vm: &Vm, v: Value) -> bool {
    matches!(v, Value::Object(o) if matches!(vm.heap.obj(o).class, Class::Promise(_)))
}

/// A fresh pending promise with the given prototype.
pub fn new_promise_object(vm: &mut Vm, proto: ObjId) -> ObjId {
    let data = PromiseData {
        status: PromiseStatus::Pending,
        result: Value::Undefined,
        fulfill_reactions: Vec::new(),
        reject_reactions: Vec::new(),
        is_handled: false,
    };
    vm.alloc(ObjectData::new(Some(proto), Class::Promise(Box::new(data))))
}

// ---- resolving functions ------------------------------------------------------------

/// CreateResolvingFunctions(promise).
pub fn create_resolving_functions(vm: &mut Vm, promise: ObjId) -> (Value, Value) {
    let flag = Rc::new(Cell::new(false));
    let resolve = vm.new_native_closure(
        "",
        1,
        promise_resolve_function,
        vec![Value::Object(promise)],
    );
    let reject =
        vm.new_native_closure("", 1, promise_reject_function, vec![Value::Object(promise)]);
    for f in [resolve, reject] {
        if let Some(n) = vm.heap.obj_mut(f).native_mut() {
            n.flag = Some(flag.clone());
        }
    }
    (Value::Object(resolve), Value::Object(reject))
}

fn take_already_resolved(vm: &mut Vm, callee: ObjId) -> bool {
    let n = vm.heap.obj(callee).native().expect("resolving function");
    let flag = n.flag.as_ref().expect("resolving function flag");
    if flag.get() {
        return true;
    }
    flag.set(true);
    false
}

fn promise_resolve_function(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if take_already_resolved(vm, a.callee) {
        return Ok(Value::Undefined);
    }
    let promise = vm.native_slot(a.callee, 0).as_object().unwrap();
    resolve_promise(vm, promise, a.arg(0))?;
    Ok(Value::Undefined)
}

fn promise_reject_function(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if take_already_resolved(vm, a.callee) {
        return Ok(Value::Undefined);
    }
    let promise = vm.native_slot(a.callee, 0).as_object().unwrap();
    reject_promise(vm, promise, a.arg(0))?;
    Ok(Value::Undefined)
}

/// The body of a promise resolve function after the already-resolved check.
pub fn resolve_promise(vm: &mut Vm, promise: ObjId, resolution: Value) -> JsResult<()> {
    if resolution == Value::Object(promise) {
        let err = vm.make_error(ErrorType::TypeError, "Chaining cycle detected for promise");
        return reject_promise(vm, promise, err);
    }
    let Value::Object(ro) = resolution else {
        return fulfill_promise(vm, promise, resolution);
    };
    let then = match vm.get_prop(ro, PropKey::Str(vm.names.then), resolution) {
        Ok(t) => t,
        Err(Fault::Throw(e)) => return reject_promise(vm, promise, e),
        Err(f) => return Err(f),
    };
    if !vm.is_callable(then) {
        return fulfill_promise(vm, promise, resolution);
    }
    vm.jobs.push_back(Job::ResolveThenable {
        promise,
        thenable: resolution,
        then,
    });
    Ok(())
}

pub fn fulfill_promise(vm: &mut Vm, promise: ObjId, value: Value) -> JsResult<()> {
    let reactions = {
        let d = promise_data_mut(vm, promise);
        debug_assert_eq!(d.status, PromiseStatus::Pending);
        d.result = value;
        d.status = PromiseStatus::Fulfilled;
        d.reject_reactions.clear();
        std::mem::take(&mut d.fulfill_reactions)
    };
    trigger_reactions(vm, reactions, value);
    Ok(())
}

pub fn reject_promise(vm: &mut Vm, promise: ObjId, reason: Value) -> JsResult<()> {
    let (reactions, handled) = {
        let d = promise_data_mut(vm, promise);
        debug_assert_eq!(d.status, PromiseStatus::Pending);
        d.result = reason;
        d.status = PromiseStatus::Rejected;
        d.fulfill_reactions.clear();
        (std::mem::take(&mut d.reject_reactions), d.is_handled)
    };
    if !handled {
        host_rejection_tracker(vm, promise, false);
    }
    trigger_reactions(vm, reactions, reason);
    Ok(())
}

fn trigger_reactions(vm: &mut Vm, reactions: Vec<Reaction>, argument: Value) {
    for r in reactions {
        vm.jobs.push_back(Job::Reaction {
            reaction: r,
            argument,
        });
    }
}

/// HostPromiseRejectionTracker: records unhandled rejections for the host.
fn host_rejection_tracker(vm: &mut Vm, promise: ObjId, handle: bool) {
    vm.rejection_events.push((promise, handle));
}

/// Deliver pending rejection events to the host (after the job queue drains).
pub fn flush_rejections(vm: &mut Vm) {
    let events = std::mem::take(&mut vm.rejection_events);
    let realm = vm.current_realm;
    for (p, handled) in events {
        let pv = vm.to_api_value(Value::Object(p));
        let host = vm.host.clone();
        if handled {
            host.promise_rejection(vm, realm, RejectionEvent::Handle { promise: pv });
        } else {
            let reason = promise_data(vm, p)
                .map(|d| d.result)
                .unwrap_or(Value::Undefined);
            let rv = vm.to_api_value(reason);
            host.promise_rejection(
                vm,
                realm,
                RejectionEvent::Reject {
                    promise: pv,
                    reason: rv,
                },
            );
        }
    }
}

// ---- capabilities ----------------------------------------------------------------------

/// NewPromiseCapability(%Promise%): no user code can observe this.
pub fn new_promise_capability_intrinsic(vm: &mut Vm) -> Capability {
    let proto = vm.intrinsic(Intrinsic::PromisePrototype);
    let promise = new_promise_object(vm, proto);
    let (resolve, reject) = create_resolving_functions(vm, promise);
    Capability {
        promise,
        resolve,
        reject,
    }
}

/// NewPromiseCapability(C).
pub fn new_promise_capability(vm: &mut Vm, c: Value) -> JsResult<Capability> {
    if !vm.is_constructor(c) {
        return Err(vm.throw_type("Promise capability constructor is not a constructor"));
    }
    if c == Value::Object(vm.intrinsic(Intrinsic::Promise)) {
        return Ok(new_promise_capability_intrinsic(vm));
    }
    // GetCapabilitiesExecutor: slots hold [resolve, reject] once set.
    let executor = vm.new_native_closure(
        "",
        2,
        capabilities_executor,
        vec![Value::Undefined, Value::Undefined],
    );
    let promise = vm.construct_value(c, &[Value::Object(executor)], c)?;
    let resolve = vm.native_slot(executor, 0);
    let reject = vm.native_slot(executor, 1);
    if !vm.is_callable(resolve) {
        return Err(vm.throw_type("Promise resolve function is not callable"));
    }
    if !vm.is_callable(reject) {
        return Err(vm.throw_type("Promise reject function is not callable"));
    }
    let Value::Object(promise) = promise else {
        unreachable!()
    };
    Ok(Capability {
        promise,
        resolve,
        reject,
    })
}

fn capabilities_executor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let cur_resolve = vm.native_slot(a.callee, 0);
    let cur_reject = vm.native_slot(a.callee, 1);
    if !cur_resolve.is_undefined() {
        return Err(
            vm.throw_type("Promise executor has already been invoked with non-undefined arguments")
        );
    }
    if !cur_reject.is_undefined() {
        return Err(
            vm.throw_type("Promise executor has already been invoked with non-undefined arguments")
        );
    }
    let n = vm.heap.obj_mut(a.callee).native_mut().unwrap();
    n.slots[0] = a.arg(0);
    n.slots[1] = a.arg(1);
    Ok(Value::Undefined)
}

pub fn resolve_capability(vm: &mut Vm, cap: &Capability, v: Value) -> JsResult<()> {
    vm.call_value(cap.resolve, Value::Undefined, &[v])?;
    Ok(())
}

pub fn reject_capability(vm: &mut Vm, cap: &Capability, v: Value) -> JsResult<()> {
    vm.call_value(cap.reject, Value::Undefined, &[v])?;
    Ok(())
}

/// PromiseResolve(%Promise%, x).
pub fn promise_resolve_intrinsic(vm: &mut Vm, x: Value) -> JsResult<ObjId> {
    let c = Value::Object(vm.intrinsic(Intrinsic::Promise));
    promise_resolve(vm, c, x)
}

/// PromiseResolve(C, x).
pub fn promise_resolve(vm: &mut Vm, c: Value, x: Value) -> JsResult<ObjId> {
    if let Value::Object(xo) = x {
        if matches!(vm.heap.obj(xo).class, Class::Promise(_)) {
            let xc = vm.get_prop(xo, PropKey::Str(vm.names.constructor), x)?;
            if xc == c {
                return Ok(xo);
            }
        }
    }
    let cap = new_promise_capability(vm, c)?;
    resolve_capability(vm, &cap, x)?;
    Ok(cap.promise)
}

/// PerformPromiseThen(promise, onFulfilled, onRejected, resultCapability).
pub fn perform_then(
    vm: &mut Vm,
    promise: ObjId,
    on_fulfilled: Value,
    on_rejected: Value,
    cap: Option<Capability>,
) -> JsResult<Value> {
    let on_fulfilled = if vm.is_callable(on_fulfilled) {
        on_fulfilled
    } else {
        Value::Undefined
    };
    let on_rejected = if vm.is_callable(on_rejected) {
        on_rejected
    } else {
        Value::Undefined
    };
    let fr = Reaction {
        capability: cap.clone(),
        kind: ReactionKind::Fulfill,
        handler: on_fulfilled,
    };
    let rr = Reaction {
        capability: cap.clone(),
        kind: ReactionKind::Reject,
        handler: on_rejected,
    };
    let (status, result, was_handled) = {
        let d = promise_data_mut(vm, promise);
        let r = (d.status, d.result, d.is_handled);
        match d.status {
            PromiseStatus::Pending => {
                d.fulfill_reactions.push(fr.clone());
                d.reject_reactions.push(rr.clone());
            }
            _ => {}
        }
        d.is_handled = true;
        r
    };
    match status {
        PromiseStatus::Pending => {}
        PromiseStatus::Fulfilled => vm.jobs.push_back(Job::Reaction {
            reaction: fr,
            argument: result,
        }),
        PromiseStatus::Rejected => {
            if !was_handled {
                host_rejection_tracker(vm, promise, true);
            }
            vm.jobs.push_back(Job::Reaction {
                reaction: rr,
                argument: result,
            });
        }
    }
    Ok(match cap {
        Some(c) => Value::Object(c.promise),
        None => Value::Undefined,
    })
}

// ---- jobs -----------------------------------------------------------------------------------

impl Vm {
    /// Run one job from the queue.
    pub fn run_job(&mut self, job: Job) -> JsResult<()> {
        match job {
            Job::Reaction { reaction, argument } => {
                let result: Result<Value, Value> = if reaction.handler.is_undefined() {
                    match reaction.kind {
                        ReactionKind::Fulfill => Ok(argument),
                        ReactionKind::Reject => Err(argument),
                    }
                } else {
                    match self.call_value(reaction.handler, Value::Undefined, &[argument]) {
                        Ok(v) => Ok(v),
                        Err(Fault::Throw(e)) => Err(e),
                        Err(f) => return Err(f),
                    }
                };
                if let Some(cap) = reaction.capability {
                    match result {
                        Ok(v) => resolve_capability(self, &cap, v)?,
                        Err(e) => reject_capability(self, &cap, e)?,
                    }
                }
                Ok(())
            }
            Job::ResolveThenable {
                promise,
                thenable,
                then,
            } => {
                let (resolve, reject) = create_resolving_functions(self, promise);
                match self.call_value(then, thenable, &[resolve, reject]) {
                    Ok(_) => Ok(()),
                    Err(Fault::Throw(e)) => {
                        self.call_value(reject, Value::Undefined, &[e])?;
                        Ok(())
                    }
                    Err(f) => Err(f),
                }
            }
            Job::Call { func, args } => {
                self.call_value(func, Value::Undefined, &args)?;
                Ok(())
            }
        }
    }

    /// Drain the job queue. Returns the number of jobs run.
    pub fn drain_jobs(&mut self) -> JsResult<usize> {
        let mut n = 0;
        while let Some(job) = self.jobs.pop_front() {
            n += 1;
            let r = self.run_job(job);
            if let Err(f) = r {
                match f {
                    // A job that throws is reported to the host as an error.
                    Fault::Throw(_) => return Err(f),
                    other => return Err(other),
                }
            }
        }
        flush_rejections(self);
        Ok(n)
    }
}

// ---- the Promise constructor and prototype ----------------------------------------------

pub fn install(vm: &mut Vm, realm: gable_engine_api::RealmId) {
    let proto = vm.new_object();
    vm.realm_mut()
        .set_intrinsic(Intrinsic::PromisePrototype, proto);
    let ctor = vm.new_native_ctor("Promise", 1, promise_constructor);
    vm.realm_mut().set_intrinsic(Intrinsic::Promise, ctor);
    vm.define_raw_str(ctor, "prototype", Value::Object(proto), Attrs::NONE);
    vm.define_raw_str(proto, "constructor", Value::Object(ctor), Attrs::METHOD);
    let tag = vm.str("Promise");
    vm.define_raw(
        proto,
        PropKey::Sym(vm.well_known.to_string_tag),
        tag,
        Attrs::CONFIGURABLE_ONLY,
    );
    let global = vm.realm_of(realm).global;
    vm.define_raw_str(global, "Promise", Value::Object(ctor), Attrs::METHOD);

    vm.method(proto, "then", 2, promise_then);
    vm.method(proto, "catch", 1, promise_catch);
    vm.method(proto, "finally", 1, promise_finally);

    let resolve = vm.method(ctor, "resolve", 1, promise_static_resolve);
    vm.realm_mut()
        .set_intrinsic(Intrinsic::PromiseResolve, resolve);
    vm.method(ctor, "reject", 1, promise_static_reject);
    vm.method(ctor, "all", 1, promise_all);
    vm.method(ctor, "allSettled", 1, promise_all_settled);
    vm.method(ctor, "any", 1, promise_any);
    vm.method(ctor, "race", 1, promise_race);
    vm.method(ctor, "withResolvers", 0, promise_with_resolvers);
    vm.method(ctor, "try", 1, promise_try);
    vm.accessor_sym(
        ctor,
        vm.well_known.species,
        "Symbol.species",
        species_getter,
    );
}

pub(crate) fn species_getter(_vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    Ok(a.this)
}

fn promise_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if !a.is_construct() {
        return Err(vm.throw_type("Promise constructor cannot be invoked without 'new'"));
    }
    let executor = a.arg(0);
    if !vm.is_callable(executor) {
        return Err(vm.throw_type("Promise resolver is not a function"));
    }
    let proto = vm.get_prototype_from_constructor(a.new_target, Intrinsic::PromisePrototype)?;
    let promise = new_promise_object(vm, proto);
    let (resolve, reject) = create_resolving_functions(vm, promise);
    if let Err(f) = vm.call_value(executor, Value::Undefined, &[resolve, reject]) {
        match f {
            Fault::Throw(e) => {
                vm.call_value(reject, Value::Undefined, &[e])?;
            }
            other => return Err(other),
        }
    }
    Ok(Value::Object(promise))
}

fn this_promise(vm: &mut Vm, this: Value, method: &str) -> JsResult<ObjId> {
    match this {
        Value::Object(o) if matches!(vm.heap.obj(o).class, Class::Promise(_)) => Ok(o),
        _ => Err(vm.throw_type(&format!(
            "Promise.prototype.{method} called on incompatible receiver"
        ))),
    }
}

fn promise_then(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let p = this_promise(vm, a.this, "then")?;
    let c = vm.species_constructor(p, Intrinsic::Promise)?;
    let cap = new_promise_capability(vm, c)?;
    perform_then(vm, p, a.arg(0), a.arg(1), Some(cap))
}

fn promise_catch(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    vm.invoke(
        a.this,
        PropKey::Str(vm.names.then),
        &[Value::Undefined, a.arg(0)],
    )
}

fn promise_finally(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Value::Object(p) = a.this else {
        return Err(vm.throw_type("Promise.prototype.finally called on non-object"));
    };
    let c = vm.species_constructor(p, Intrinsic::Promise)?;
    let on_finally = a.arg(0);
    let (then_finally, catch_finally) = if !vm.is_callable(on_finally) {
        (on_finally, on_finally)
    } else {
        let tf = vm.new_native_closure("", 1, then_finally_fn, vec![on_finally, c]);
        let cf = vm.new_native_closure("", 1, catch_finally_fn, vec![on_finally, c]);
        (Value::Object(tf), Value::Object(cf))
    };
    vm.invoke(
        a.this,
        PropKey::Str(vm.names.then),
        &[then_finally, catch_finally],
    )
}

fn then_finally_fn(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let on_finally = vm.native_slot(a.callee, 0);
    let c = vm.native_slot(a.callee, 1);
    let result = vm.call_value(on_finally, Value::Undefined, &[])?;
    let p = promise_resolve(vm, c, result)?;
    let value_thunk = vm.new_native_closure("", 0, value_thunk_fn, vec![a.arg(0)]);
    vm.invoke(
        Value::Object(p),
        PropKey::Str(vm.names.then),
        &[Value::Object(value_thunk)],
    )
}

fn catch_finally_fn(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let on_finally = vm.native_slot(a.callee, 0);
    let c = vm.native_slot(a.callee, 1);
    let result = vm.call_value(on_finally, Value::Undefined, &[])?;
    let p = promise_resolve(vm, c, result)?;
    let thrower = vm.new_native_closure("", 0, thrower_fn, vec![a.arg(0)]);
    vm.invoke(
        Value::Object(p),
        PropKey::Str(vm.names.then),
        &[Value::Object(thrower)],
    )
}

fn value_thunk_fn(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    Ok(vm.native_slot(a.callee, 0))
}

fn thrower_fn(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    Err(Fault::Throw(vm.native_slot(a.callee, 0)))
}

fn promise_static_resolve(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if !a.this.is_object() {
        return Err(vm.throw_type("Promise.resolve called on non-object"));
    }
    Ok(Value::Object(promise_resolve(vm, a.this, a.arg(0))?))
}

fn promise_static_reject(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let cap = new_promise_capability(vm, a.this)?;
    reject_capability(vm, &cap, a.arg(0))?;
    Ok(Value::Object(cap.promise))
}

fn promise_with_resolvers(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let cap = new_promise_capability(vm, a.this)?;
    let o = vm.new_object();
    vm.define_raw_str(o, "promise", Value::Object(cap.promise), Attrs::ALL);
    vm.define_raw_str(o, "resolve", cap.resolve, Attrs::ALL);
    vm.define_raw_str(o, "reject", cap.reject, Attrs::ALL);
    Ok(Value::Object(o))
}

fn promise_try(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if !a.this.is_object() {
        return Err(vm.throw_type("Promise.try called on non-object"));
    }
    if !vm.is_constructor(a.this) {
        return Err(vm.throw_type("Promise.try called on a non-constructor"));
    }
    let args: Vec<Value> = a.args.iter().skip(1).copied().collect();
    match vm.call_value(a.arg(0), Value::Undefined, &args) {
        Ok(v) => Ok(Value::Object(promise_resolve(vm, a.this, v)?)),
        Err(Fault::Throw(e)) => {
            let cap = new_promise_capability(vm, a.this)?;
            reject_capability(vm, &cap, e)?;
            Ok(Value::Object(cap.promise))
        }
        Err(f) => Err(f),
    }
}

/// GetPromiseResolve(C).
fn get_promise_resolve(vm: &mut Vm, c: Value) -> JsResult<Value> {
    let r = vm.get_value(c, PropKey::Str(vm.names.resolve))?;
    if !vm.is_callable(r) {
        return Err(vm.throw_type("Promise resolve is not a function"));
    }
    Ok(r)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Combinator {
    All,
    AllSettled,
    Any,
}

/// Why an aggregate combinator stopped early.
enum CombAbrupt {
    /// The iterator itself failed: do not close it.
    Iterator(Fault),
    /// Something else failed while the iterator was live: close it first.
    Other(Fault),
}

/// The shared driver of Promise.all / allSettled / any.
fn promise_combinator(vm: &mut Vm, a: &CallArgs, which: Combinator) -> JsResult<Value> {
    let c = a.this;
    let cap = new_promise_capability(vm, c)?;
    let reject_with = |vm: &mut Vm, f: Fault| -> JsResult<Value> {
        match f {
            Fault::Throw(e) => {
                vm.call_value(cap.reject, Value::Undefined, &[e])?;
                Ok(Value::Object(cap.promise))
            }
            other => Err(other),
        }
    };
    let resolve_fn = match get_promise_resolve(vm, c) {
        Ok(f) => f,
        Err(f) => return reject_with(vm, f),
    };
    let (iter, next) = match vm.get_iterator(a.arg(0), false) {
        Ok(r) => r,
        Err(f) => return reject_with(vm, f),
    };
    match combinator_loop(vm, which, c, &cap, resolve_fn, iter, next) {
        Ok(()) => Ok(Value::Object(cap.promise)),
        Err(CombAbrupt::Iterator(f)) => reject_with(vm, f),
        Err(CombAbrupt::Other(f)) => {
            if matches!(f, Fault::Throw(_)) {
                vm.iterator_close_swallow(iter);
            }
            reject_with(vm, f)
        }
    }
}

fn combinator_loop(
    vm: &mut Vm,
    which: Combinator,
    c: Value,
    cap: &Capability,
    resolve_fn: Value,
    iter: Value,
    next: Value,
) -> Result<(), CombAbrupt> {
    let values = vm.new_array(Vec::new());
    let remaining = Rc::new(Cell::new(1usize));
    let mut index = 0u32;
    loop {
        let item = match vm.iterator_step_value(iter, next) {
            Ok(Some(v)) => v,
            Ok(None) => break,
            Err(f) => return Err(CombAbrupt::Iterator(f)),
        };
        vm.create_data_property_or_throw(values, PropKey::Index(index), Value::Undefined)
            .map_err(CombAbrupt::Other)?;
        let next_promise = vm
            .call_value(resolve_fn, c, &[item])
            .map_err(CombAbrupt::Other)?;
        remaining.set(remaining.get() + 1);
        let already = Rc::new(Cell::new(false));
        let idx = Value::Number(index as f64);
        let make = |vm: &mut Vm, f: crate::object::NativeFn| -> Value {
            let o = vm.new_native_closure(
                "",
                1,
                f,
                vec![idx, Value::Object(values), cap.resolve, cap.reject],
            );
            let n = vm.heap.obj_mut(o).native_mut().unwrap();
            n.flag = Some(already.clone());
            n.counter = Some(remaining.clone());
            Value::Object(o)
        };
        let (on_ok, on_err) = match which {
            Combinator::All => (make(vm, all_resolve_element), cap.reject),
            Combinator::AllSettled => (
                make(vm, all_settled_fulfilled),
                make(vm, all_settled_rejected),
            ),
            Combinator::Any => (cap.resolve, make(vm, any_reject_element)),
        };
        vm.invoke(next_promise, PropKey::Str(vm.names.then), &[on_ok, on_err])
            .map_err(CombAbrupt::Other)?;
        index += 1;
    }
    remaining.set(remaining.get() - 1);
    if remaining.get() == 0 {
        match which {
            Combinator::Any => {
                let err = make_aggregate_error(vm, values);
                vm.call_value(cap.reject, Value::Undefined, &[err])
                    .map_err(CombAbrupt::Iterator)?;
            }
            _ => {
                vm.call_value(cap.resolve, Value::Undefined, &[Value::Object(values)])
                    .map_err(CombAbrupt::Iterator)?;
            }
        }
    }
    Ok(())
}

fn make_aggregate_error(vm: &mut Vm, errors: ObjId) -> Value {
    let e = vm.make_error(ErrorType::AggregateError, "All promises were rejected");
    if let Value::Object(eo) = e {
        let k = PropKey::Str(vm.names.errors);
        let arr = vm.create_array_copy(errors);
        vm.define_raw(eo, k, Value::Object(arr), Attrs::METHOD);
    }
    e
}

fn element_common(
    vm: &mut Vm,
    callee: ObjId,
) -> Option<(u32, ObjId, Value, Value, Rc<Cell<usize>>)> {
    let n = vm.heap.obj(callee).native().unwrap();
    let flag = n.flag.as_ref().unwrap();
    if flag.get() {
        return None;
    }
    flag.set(true);
    let remaining = n.counter.clone().unwrap();
    let idx = n.slots[0].as_number().unwrap() as u32;
    let values = n.slots[1].as_object().unwrap();
    Some((idx, values, n.slots[2], n.slots[3], remaining))
}

fn all_resolve_element(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Some((idx, values, resolve, _, remaining)) = element_common(vm, a.callee) else {
        return Ok(Value::Undefined);
    };
    vm.create_data_property_or_throw(values, PropKey::Index(idx), a.arg(0))?;
    remaining.set(remaining.get() - 1);
    if remaining.get() == 0 {
        vm.call_value(resolve, Value::Undefined, &[Value::Object(values)])?;
    }
    Ok(Value::Undefined)
}

fn settled_record(vm: &mut Vm, fulfilled: bool, v: Value) -> Value {
    let o = vm.new_object();
    let status = if fulfilled {
        vm.names.fulfilled
    } else {
        vm.names.rejected
    };
    vm.define_raw_str(o, "status", Value::String(status), Attrs::ALL);
    if fulfilled {
        vm.define_raw_str(o, "value", v, Attrs::ALL);
    } else {
        vm.define_raw_str(o, "reason", v, Attrs::ALL);
    }
    Value::Object(o)
}

fn all_settled_fulfilled(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Some((idx, values, resolve, _, remaining)) = element_common(vm, a.callee) else {
        return Ok(Value::Undefined);
    };
    let rec = settled_record(vm, true, a.arg(0));
    vm.create_data_property_or_throw(values, PropKey::Index(idx), rec)?;
    remaining.set(remaining.get() - 1);
    if remaining.get() == 0 {
        vm.call_value(resolve, Value::Undefined, &[Value::Object(values)])?;
    }
    Ok(Value::Undefined)
}

fn all_settled_rejected(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Some((idx, values, resolve, _, remaining)) = element_common(vm, a.callee) else {
        return Ok(Value::Undefined);
    };
    let rec = settled_record(vm, false, a.arg(0));
    vm.create_data_property_or_throw(values, PropKey::Index(idx), rec)?;
    remaining.set(remaining.get() - 1);
    if remaining.get() == 0 {
        vm.call_value(resolve, Value::Undefined, &[Value::Object(values)])?;
    }
    Ok(Value::Undefined)
}

fn any_reject_element(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Some((idx, values, _, reject, remaining)) = element_common(vm, a.callee) else {
        return Ok(Value::Undefined);
    };
    vm.create_data_property_or_throw(values, PropKey::Index(idx), a.arg(0))?;
    remaining.set(remaining.get() - 1);
    if remaining.get() == 0 {
        let err = make_aggregate_error(vm, values);
        vm.call_value(reject, Value::Undefined, &[err])?;
    }
    Ok(Value::Undefined)
}

fn promise_all(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    promise_combinator(vm, a, Combinator::All)
}

fn promise_all_settled(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    promise_combinator(vm, a, Combinator::AllSettled)
}

fn promise_any(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    promise_combinator(vm, a, Combinator::Any)
}

fn promise_race(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let c = a.this;
    let cap = new_promise_capability(vm, c)?;
    let reject_with = |vm: &mut Vm, f: Fault| -> JsResult<Value> {
        match f {
            Fault::Throw(e) => {
                vm.call_value(cap.reject, Value::Undefined, &[e])?;
                Ok(Value::Object(cap.promise))
            }
            other => Err(other),
        }
    };
    let resolve_fn = match get_promise_resolve(vm, c) {
        Ok(f) => f,
        Err(f) => return reject_with(vm, f),
    };
    let (iter, next) = match vm.get_iterator(a.arg(0), false) {
        Ok(r) => r,
        Err(f) => return reject_with(vm, f),
    };
    let body = |vm: &mut Vm| -> Result<(), CombAbrupt> {
        loop {
            let item = match vm.iterator_step_value(iter, next) {
                Ok(Some(v)) => v,
                Ok(None) => return Ok(()),
                Err(f) => return Err(CombAbrupt::Iterator(f)),
            };
            let next_promise = vm
                .call_value(resolve_fn, c, &[item])
                .map_err(CombAbrupt::Other)?;
            vm.invoke(
                next_promise,
                PropKey::Str(vm.names.then),
                &[cap.resolve, cap.reject],
            )
            .map_err(CombAbrupt::Other)?;
        }
    };
    match body(vm) {
        Ok(()) => Ok(Value::Object(cap.promise)),
        Err(CombAbrupt::Iterator(f)) => reject_with(vm, f),
        Err(CombAbrupt::Other(f)) => {
            if matches!(f, Fault::Throw(_)) {
                vm.iterator_close_swallow(iter);
            }
            reject_with(vm, f)
        }
    }
}

impl Vm {
    /// A fresh array with the same elements as `arr` (dense arrays).
    pub fn create_array_copy(&mut self, arr: ObjId) -> ObjId {
        let items = self.array_to_vec(arr);
        self.new_array(items)
    }
}
