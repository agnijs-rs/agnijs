//! Array.fromAsync (ES2026).
//!
//! The spec runs an async abstract closure under AsyncFunctionStart. Here
//! the closure is a native state machine: a hidden native function, the
//! driver, carries the state in its captured slots, and every Await
//! resolves the awaited value to a promise and attaches two small native
//! closures that point back at the driver and resume it with the outcome.
//! The driver itself is never called and never reaches script; it lives as
//! long as a pending reaction references it.

use crate::builtins::promise::{self, Capability};
use crate::object::CallArgs;
use crate::value::{ObjId, PropKey, Value};
use crate::vm::{ErrorType, Fault, JsResult, Vm};

const MAX_SAFE: f64 = 9007199254740991.0;

// Driver slots.
const S_C: usize = 0;
const S_ARRAY: usize = 1;
/// The iterator (iterable mode) or the array-like object.
const S_SOURCE: usize = 2;
const S_NEXT: usize = 3;
const S_MAPFN: usize = 4;
const S_THIS_ARG: usize = 5;
const S_K: usize = 6;
/// `undefined` in iterable mode, the length in array-like mode.
const S_LEN: usize = 7;
const S_PROMISE: usize = 8;
const S_PHASE: usize = 9;
/// The throw completion an AsyncIteratorClose is carrying.
const S_ERROR: usize = 10;
const SLOTS: usize = 11;

// What the pending Await resumes into.
/// The result of `next()` (iterable mode).
const P_NEXT: f64 = 0.0;
/// The mapped value (iterable mode).
const P_ITER_MAPPED: f64 = 1.0;
/// The `return()` result of AsyncIteratorClose, carrying S_ERROR.
const P_CLOSE: f64 = 2.0;
/// The element value (array-like mode).
const P_VALUE: f64 = 3.0;
/// The mapped value (array-like mode).
const P_LIKE_MAPPED: f64 = 4.0;

/// What a step of the state machine ended with.
enum Step {
    /// An Await is pending; the driver resumes later.
    Await,
    /// The closure returned the array.
    Done(Value),
}

pub fn array_from_async(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let cap = promise::new_internal_capability_intrinsic(vm);
    let driver = vm.new_native_closure("", 0, driver_never_called, vec![Value::Undefined; SLOTS]);
    vm.set_native_slot(driver, S_PROMISE, Value::Object(cap.promise));
    vm.set_native_slot(driver, S_C, a.this);
    let r = start(vm, driver, a.arg(0), a.arg(1), a.arg(2));
    finish(vm, driver, r)?;
    Ok(Value::Object(cap.promise))
}

fn driver_never_called(_vm: &mut Vm, _a: &CallArgs) -> JsResult<Value> {
    Ok(Value::Undefined)
}

fn slot(vm: &Vm, d: ObjId, i: usize) -> Value {
    vm.native_slot(d, i)
}

fn capability(vm: &Vm, d: ObjId) -> Capability {
    let Value::Object(promise) = slot(vm, d, S_PROMISE) else {
        unreachable!("the driver holds its promise")
    };
    let realm = vm.heap.obj(d).native().map(|n| n.realm).unwrap_or(vm.current_realm);
    Capability { promise, realm, resolve: Value::Undefined, reject: Value::Undefined }
}

/// Settle the returned promise when the closure has returned or thrown.
fn finish(vm: &mut Vm, d: ObjId, r: JsResult<Step>) -> JsResult<()> {
    let cap = capability(vm, d);
    match r {
        Ok(Step::Await) => Ok(()),
        Ok(Step::Done(v)) => promise::resolve_capability(vm, &cap, v),
        Err(Fault::Throw(e)) => promise::reject_capability(vm, &cap, e),
        Err(f) => Err(f),
    }
}

/// Await(v): resume the driver in `phase` once `v` settles.
fn await_value(vm: &mut Vm, d: ObjId, phase: f64, v: Value) -> JsResult<Step> {
    let p = promise::promise_resolve_intrinsic(vm, v)?;
    vm.set_native_slot(d, S_PHASE, Value::Number(phase));
    let on_ok = vm.new_native_closure("", 1, resume_fulfilled, vec![Value::Object(d)]);
    let on_err = vm.new_native_closure("", 1, resume_rejected, vec![Value::Object(d)]);
    promise::perform_then(vm, p, Value::Object(on_ok), Value::Object(on_err), None)?;
    Ok(Step::Await)
}

fn resume_fulfilled(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    resume(vm, a, Ok(a.arg(0)))
}

fn resume_rejected(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    resume(vm, a, Err(a.arg(0)))
}

fn resume(vm: &mut Vm, a: &CallArgs, outcome: Result<Value, Value>) -> JsResult<Value> {
    let Value::Object(d) = vm.native_slot(a.callee, 0) else {
        return Ok(Value::Undefined);
    };
    let r = continue_from(vm, d, outcome);
    finish(vm, d, r)?;
    Ok(Value::Undefined)
}

fn num(vm: &Vm, d: ObjId, i: usize) -> f64 {
    match slot(vm, d, i) {
        Value::Number(n) => n,
        _ => 0.0,
    }
}

/// The closure up to its first Await.
fn start(vm: &mut Vm, d: ObjId, items: Value, mapfn: Value, this_arg: Value) -> JsResult<Step> {
    if !mapfn.is_undefined() && !vm.is_callable(mapfn) {
        return vm.type_error("Array.fromAsync: mapper is not a function");
    }
    vm.set_native_slot(d, S_MAPFN, mapfn);
    vm.set_native_slot(d, S_THIS_ARG, this_arg);
    let c = slot(vm, d, S_C);
    let async_method = vm.get_method(items, PropKey::Sym(vm.well_known.async_iterator))?;
    let sync_method = match async_method {
        Some(_) => None,
        None => vm.get_method(items, PropKey::Sym(vm.well_known.iterator))?,
    };
    let record = match (async_method, sync_method) {
        (Some(m), _) => {
            let it = vm.call_value(m, items, &[])?;
            if !it.is_object() {
                return vm.type_error("Result of the Symbol.asyncIterator method is not an object");
            }
            let next = vm.get_value(it, PropKey::Str(vm.names.next))?;
            Some((it, next))
        }
        (None, Some(m)) => {
            let it = vm.call_value(m, items, &[])?;
            if !it.is_object() {
                return vm.type_error("Result of the Symbol.iterator method is not an object");
            }
            let next = vm.get_value(it, PropKey::Str(vm.names.next))?;
            let wrapped = crate::builtins::iterator::create_async_from_sync_iterator(vm, it, next);
            let next = vm.get_value(wrapped, PropKey::Str(vm.names.next))?;
            Some((wrapped, next))
        }
        (None, None) => None,
    };
    if let Some((it, next)) = record {
        let arr = if vm.is_constructor(c) {
            vm.construct_value(c, &[], c)?
        } else {
            Value::Object(vm.array_create(0.0, None)?)
        };
        vm.set_native_slot(d, S_ARRAY, arr);
        vm.set_native_slot(d, S_SOURCE, it);
        vm.set_native_slot(d, S_NEXT, next);
        vm.set_native_slot(d, S_K, Value::Number(0.0));
        return iter_next(vm, d);
    }
    let like = vm.to_object(items)?;
    let len = vm.length_of_array_like(like)?;
    let arr = if vm.is_constructor(c) {
        vm.construct_value(c, &[Value::Number(len)], c)?
    } else {
        Value::Object(vm.array_create(len, None)?)
    };
    vm.set_native_slot(d, S_ARRAY, arr);
    vm.set_native_slot(d, S_SOURCE, Value::Object(like));
    vm.set_native_slot(d, S_LEN, Value::Number(len));
    vm.set_native_slot(d, S_K, Value::Number(0.0));
    like_next(vm, d)
}

fn continue_from(vm: &mut Vm, d: ObjId, outcome: Result<Value, Value>) -> JsResult<Step> {
    let phase = num(vm, d, S_PHASE);
    if phase == P_NEXT {
        let r = outcome.map_err(Fault::Throw)?;
        let Value::Object(ro) = r else {
            return vm.type_error("Iterator result is not an object");
        };
        let done = vm.get(ro, PropKey::Str(vm.names.done))?;
        let k = num(vm, d, S_K);
        if vm.to_boolean(done) {
            let arr = slot(vm, d, S_ARRAY);
            set_length(vm, arr, k)?;
            return Ok(Step::Done(arr));
        }
        let v = vm.get(ro, PropKey::Str(vm.names.value))?;
        let mapfn = slot(vm, d, S_MAPFN);
        if mapfn.is_undefined() {
            return iter_store(vm, d, v);
        }
        let this_arg = slot(vm, d, S_THIS_ARG);
        let mapped = match vm.call_value(mapfn, this_arg, &[v, Value::Number(k)]) {
            Ok(m) => m,
            Err(f) => return async_close(vm, d, f),
        };
        return match await_value(vm, d, P_ITER_MAPPED, mapped) {
            Ok(s) => Ok(s),
            Err(f) => async_close(vm, d, f),
        };
    }
    if phase == P_ITER_MAPPED {
        return match outcome {
            Ok(v) => iter_store(vm, d, v),
            Err(e) => async_close(vm, d, Fault::Throw(e)),
        };
    }
    if phase == P_CLOSE {
        // AsyncIteratorClose with a throw completion: the original error
        // wins over whatever `return()` produced.
        return Err(Fault::Throw(slot(vm, d, S_ERROR)));
    }
    if phase == P_VALUE {
        let v = outcome.map_err(Fault::Throw)?;
        let mapfn = slot(vm, d, S_MAPFN);
        if mapfn.is_undefined() {
            return like_store(vm, d, v);
        }
        let k = num(vm, d, S_K);
        let this_arg = slot(vm, d, S_THIS_ARG);
        let mapped = vm.call_value(mapfn, this_arg, &[v, Value::Number(k)])?;
        return await_value(vm, d, P_LIKE_MAPPED, mapped);
    }
    // P_LIKE_MAPPED
    let v = outcome.map_err(Fault::Throw)?;
    like_store(vm, d, v)
}

/// The top of the iterable loop: call `next()` and await its result.
fn iter_next(vm: &mut Vm, d: ObjId) -> JsResult<Step> {
    let k = num(vm, d, S_K);
    if k >= MAX_SAFE {
        let e = vm.make_error(ErrorType::TypeError, "Array.fromAsync: too many elements");
        return async_close(vm, d, Fault::Throw(e));
    }
    let (it, next) = (slot(vm, d, S_SOURCE), slot(vm, d, S_NEXT));
    let r = vm.call_value(next, it, &[])?;
    await_value(vm, d, P_NEXT, r)
}

fn iter_store(vm: &mut Vm, d: ObjId, v: Value) -> JsResult<Step> {
    let k = num(vm, d, S_K);
    let arr = slot(vm, d, S_ARRAY);
    if let Err(f) = define(vm, arr, k, v) {
        return async_close(vm, d, f);
    }
    vm.set_native_slot(d, S_K, Value::Number(k + 1.0));
    iter_next(vm, d)
}

/// The top of the array-like loop: read element k and await it.
fn like_next(vm: &mut Vm, d: ObjId) -> JsResult<Step> {
    let k = num(vm, d, S_K);
    let len = num(vm, d, S_LEN);
    let arr = slot(vm, d, S_ARRAY);
    if k >= len {
        set_length(vm, arr, len)?;
        return Ok(Step::Done(arr));
    }
    let Value::Object(like) = slot(vm, d, S_SOURCE) else {
        unreachable!("array-like mode holds an object")
    };
    let key = vm.number_key(k);
    let v = vm.get(like, key)?;
    await_value(vm, d, P_VALUE, v)
}

fn like_store(vm: &mut Vm, d: ObjId, v: Value) -> JsResult<Step> {
    let k = num(vm, d, S_K);
    let arr = slot(vm, d, S_ARRAY);
    define(vm, arr, k, v)?;
    vm.set_native_slot(d, S_K, Value::Number(k + 1.0));
    like_next(vm, d)
}

fn define(vm: &mut Vm, arr: Value, k: f64, v: Value) -> JsResult<()> {
    let Value::Object(o) = arr else {
        return vm.type_error("Array.fromAsync: the constructor returned a non-object");
    };
    let key = vm.number_key(k);
    vm.create_data_property_or_throw(o, key, v)
}

fn set_length(vm: &mut Vm, arr: Value, len: f64) -> JsResult<()> {
    let key = PropKey::Str(vm.names.length);
    match arr {
        Value::Object(o) => vm.set(o, key, Value::Number(len), true),
        _ => vm.type_error("Array.fromAsync: the constructor returned a non-object"),
    }
}

/// AsyncIteratorClose(iteratorRecord, throw completion `f`): call
/// `return()` and await its result before rethrowing; a missing or
/// throwing `return` rethrows at once.
fn async_close(vm: &mut Vm, d: ObjId, f: Fault) -> JsResult<Step> {
    let Fault::Throw(e) = f else { return Err(f) };
    let it = slot(vm, d, S_SOURCE);
    let ret = match vm.get_method(it, PropKey::Str(vm.names.return_)) {
        Ok(Some(r)) => r,
        Ok(None) | Err(Fault::Throw(_)) => return Err(Fault::Throw(e)),
        Err(other) => return Err(other),
    };
    let r = match vm.call_value(ret, it, &[]) {
        Ok(r) => r,
        Err(Fault::Throw(_)) => return Err(Fault::Throw(e)),
        Err(other) => return Err(other),
    };
    vm.set_native_slot(d, S_ERROR, e);
    match await_value(vm, d, P_CLOSE, r) {
        Ok(s) => Ok(s),
        Err(Fault::Throw(_)) => Err(Fault::Throw(e)),
        Err(other) => Err(other),
    }
}

