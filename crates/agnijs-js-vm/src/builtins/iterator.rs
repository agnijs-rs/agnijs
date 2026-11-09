//! %IteratorPrototype%, %AsyncIteratorPrototype%, array and string iterators
//! and async-from-sync iterators. The iterator helpers land in M3.

use crate::builtins::promise::{self, Capability};
use crate::gc::Tracer;
use crate::object::{CallArgs, Class, IterKind, ObjectData};
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, PropKey, StrId, Value};
use crate::vm::{ErrorType, Fault, JsResult, Vm};

pub struct HelperState {
    pub underlying: Value,
    pub next: Value,
    pub kind: u8,
    pub fns: Vec<Value>,
    pub counter: f64,
    pub done: bool,
    pub executing: bool,
    pub inner: Option<(Value, Value)>,
}

impl HelperState {
    pub fn trace(&self, t: &mut Tracer) {
        t.value(self.underlying);
        t.value(self.next);
        for f in &self.fns {
            t.value(*f);
        }
        if let Some((a, b)) = self.inner {
            t.value(a);
            t.value(b);
        }
    }
}

pub fn install(vm: &mut Vm, _realm: agnijs_engine_api::RealmId) {
    let iter_proto = vm.new_object();
    vm.realm_mut()
        .set_intrinsic(Intrinsic::IteratorPrototype, iter_proto);
    let sym = vm.well_known.iterator;
    vm.method_sym(iter_proto, sym, "Symbol.iterator", 0, return_this);

    let async_iter_proto = vm.new_object();
    vm.realm_mut()
        .set_intrinsic(Intrinsic::AsyncIteratorPrototype, async_iter_proto);
    let sym = vm.well_known.async_iterator;
    vm.method_sym(
        async_iter_proto,
        sym,
        "Symbol.asyncIterator",
        0,
        return_this,
    );

    let array_iter_proto = vm.new_object_with_proto(Some(iter_proto));
    vm.realm_mut()
        .set_intrinsic(Intrinsic::ArrayIteratorPrototype, array_iter_proto);
    vm.method(array_iter_proto, "next", 0, array_iterator_next);
    tag(vm, array_iter_proto, "Array Iterator");

    let string_iter_proto = vm.new_object_with_proto(Some(iter_proto));
    vm.realm_mut()
        .set_intrinsic(Intrinsic::StringIteratorPrototype, string_iter_proto);
    vm.method(string_iter_proto, "next", 0, string_iterator_next);
    tag(vm, string_iter_proto, "String Iterator");

    let for_in_proto = vm.new_object_with_proto(Some(iter_proto));
    vm.realm_mut()
        .set_intrinsic(Intrinsic::ForInIteratorPrototype, for_in_proto);

    let afs_proto = vm.new_object_with_proto(Some(async_iter_proto));
    vm.realm_mut()
        .set_intrinsic(Intrinsic::AsyncFromSyncIteratorPrototype, afs_proto);
    vm.method(afs_proto, "next", 1, afs_next);
    vm.method(afs_proto, "return", 1, afs_return);
    vm.method(afs_proto, "throw", 1, afs_throw);

    // Placeholders so the intrinsics exist before M3 fills them in.
    let helper_proto = vm.new_object_with_proto(Some(iter_proto));
    vm.realm_mut()
        .set_intrinsic(Intrinsic::IteratorHelperPrototype, helper_proto);
    let wrap_proto = vm.new_object_with_proto(Some(iter_proto));
    vm.realm_mut()
        .set_intrinsic(Intrinsic::WrapForValidIteratorPrototype, wrap_proto);
}

/// Define `@@toStringTag` on a prototype.
pub fn tag(vm: &mut Vm, proto: ObjId, name: &str) {
    let v = vm.str(name);
    let key = PropKey::Sym(vm.well_known.to_string_tag);
    vm.define_raw(proto, key, v, Attrs::CONFIGURABLE_ONLY);
}

pub fn return_this(_vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    Ok(a.this)
}

/// CreateArrayIterator(array, kind).
pub fn create_array_iterator(vm: &mut Vm, array: ObjId, kind: IterKind) -> Value {
    let proto = vm.intrinsic(Intrinsic::ArrayIteratorPrototype);
    let state = crate::object::ArrayIterState {
        target: Some(array),
        index: 0,
        kind,
    };
    Value::Object(vm.alloc(ObjectData::new(Some(proto), Class::ArrayIterator(state))))
}

pub fn array_iterator_next(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Value::Object(o) = a.this else {
        return vm.type_error("%ArrayIteratorPrototype%.next called on incompatible receiver");
    };
    let (target, index, kind) = match &vm.heap.obj(o).class {
        Class::ArrayIterator(s) => (s.target, s.index, s.kind),
        _ => return vm.type_error("%ArrayIteratorPrototype%.next called on incompatible receiver"),
    };
    let Some(t) = target else {
        return Ok(vm.create_iter_result(Value::Undefined, true));
    };
    let len = if matches!(vm.heap.obj(t).class, Class::TypedArray(_)) {
        match crate::builtins::typedarray::ta_length(vm, t) {
            Some(n) => n as f64,
            None => return vm.type_error("Cannot perform %ArrayIteratorPrototype%.next on a detached or out-of-bounds TypedArray"),
        }
    } else {
        vm.length_of_array_like(t)?
    };
    if (index as f64) >= len {
        if let Class::ArrayIterator(s) = &mut vm.heap.obj_mut(o).class {
            s.target = None;
        }
        return Ok(vm.create_iter_result(Value::Undefined, true));
    }
    if let Class::ArrayIterator(s) = &mut vm.heap.obj_mut(o).class {
        s.index = index + 1;
    }
    let result = match kind {
        IterKind::Keys => Value::Number(index as f64),
        IterKind::Values => vm.get(t, PropKey::Index(index))?,
        IterKind::Entries => {
            let v = vm.get(t, PropKey::Index(index))?;
            Value::Object(vm.new_array(vec![Value::Number(index as f64), v]))
        }
    };
    Ok(vm.create_iter_result(result, false))
}

pub fn create_string_iterator(vm: &mut Vm, s: StrId) -> Value {
    let proto = vm.intrinsic(Intrinsic::StringIteratorPrototype);
    Value::Object(vm.alloc(ObjectData::new(
        Some(proto),
        Class::StringIterator {
            string: Some(s),
            index: 0,
        },
    )))
}

fn string_iterator_next(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Value::Object(o) = a.this else {
        return vm.type_error("%StringIteratorPrototype%.next called on incompatible receiver");
    };
    let (string, index) = match &vm.heap.obj(o).class {
        Class::StringIterator { string, index } => (*string, *index),
        _ => {
            return vm.type_error("%StringIteratorPrototype%.next called on incompatible receiver")
        }
    };
    let Some(s) = string else {
        return Ok(vm.create_iter_result(Value::Undefined, true));
    };
    let len = vm.heap.str_len(s);
    let index = index as usize;
    if index >= len {
        if let Class::StringIterator { string, .. } = &mut vm.heap.obj_mut(o).class {
            *string = None;
        }
        return Ok(vm.create_iter_result(Value::Undefined, true));
    }
    vm.heap.flatten(s);
    let (_, size) = vm.heap.string(s).units().code_point_at(index);
    let units = vm
        .heap
        .string(s)
        .units()
        .slice(index, index + size)
        .to_vec();
    if let Class::StringIterator { index: i, .. } = &mut vm.heap.obj_mut(o).class {
        *i = (index + size) as u32;
    }
    let v = vm.str_from_units(units);
    Ok(vm.create_iter_result(v, false))
}

// ---- async-from-sync ------------------------------------------------------------------------

pub fn create_async_from_sync_iterator(vm: &mut Vm, iterator: Value, next: Value) -> Value {
    let proto = vm.intrinsic(Intrinsic::AsyncFromSyncIteratorPrototype);
    Value::Object(vm.alloc(ObjectData::new(
        Some(proto),
        Class::AsyncFromSyncIterator(Box::new(crate::object::IteratorRecord { iterator, next })),
    )))
}

fn afs_parts(vm: &mut Vm, this: Value) -> (Value, Value) {
    match this {
        Value::Object(o) => match &vm.heap.obj(o).class {
            Class::AsyncFromSyncIterator(r) => (r.iterator, r.next),
            _ => unreachable!("AsyncFromSyncIterator method on wrong receiver"),
        },
        _ => unreachable!("AsyncFromSyncIterator method on wrong receiver"),
    }
}

/// Reject `cap` with the thrown value, or propagate a non-throw fault.
fn reject_or_propagate(vm: &mut Vm, cap: &Capability, f: Fault) -> JsResult<Value> {
    match f {
        Fault::Throw(e) => {
            promise::reject_capability(vm, cap, e)?;
            Ok(Value::Object(cap.promise))
        }
        other => Err(other),
    }
}

/// AsyncFromSyncIteratorContinuation.
fn afs_continuation(
    vm: &mut Vm,
    result: Value,
    cap: &Capability,
    sync_iter: Value,
    close_on_rejection: bool,
) -> JsResult<Value> {
    let Value::Object(ro) = result else {
        let e = vm.make_error(ErrorType::TypeError, "iterator result is not an object");
        promise::reject_capability(vm, cap, e)?;
        return Ok(Value::Object(cap.promise));
    };
    let done = match vm.get(ro, PropKey::Str(vm.names.done)) {
        Ok(d) => vm.to_boolean(d),
        Err(f) => return reject_or_propagate(vm, cap, f),
    };
    let value = match vm.get(ro, PropKey::Str(vm.names.value)) {
        Ok(v) => v,
        Err(f) => return reject_or_propagate(vm, cap, f),
    };
    let value_wrapper = match promise::promise_resolve_intrinsic(vm, value) {
        Ok(p) => p,
        Err(Fault::Throw(e)) => {
            if !done && close_on_rejection {
                vm.iterator_close_swallow(sync_iter);
            }
            promise::reject_capability(vm, cap, e)?;
            return Ok(Value::Object(cap.promise));
        }
        Err(f) => return Err(f),
    };
    let unwrap = vm.new_native_closure("", 1, afs_unwrap, vec![Value::Bool(done)]);
    let on_rejected = if done || !close_on_rejection {
        Value::Undefined
    } else {
        Value::Object(vm.new_native_closure("", 1, afs_close_iterator, vec![sync_iter]))
    };
    promise::perform_then(
        vm,
        value_wrapper,
        Value::Object(unwrap),
        on_rejected,
        Some(cap.clone()),
    )?;
    Ok(Value::Object(cap.promise))
}

fn afs_unwrap(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let done = vm.native_slot(a.callee, 0) == Value::Bool(true);
    Ok(vm.create_iter_result(a.arg(0), done))
}

fn afs_close_iterator(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let it = vm.native_slot(a.callee, 0);
    vm.iterator_close_swallow(it);
    Err(Fault::Throw(a.arg(0)))
}

fn afs_next(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (iter, next) = afs_parts(vm, a.this);
    let cap = promise::new_promise_capability_intrinsic(vm);
    let args: Vec<Value> = a.args.iter().take(1).copied().collect();
    let result = match vm.call_value(next, iter, &args) {
        Ok(r) => r,
        Err(f) => return reject_or_propagate(vm, &cap, f),
    };
    afs_continuation(vm, result, &cap, iter, true)
}

fn afs_return(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (iter, _) = afs_parts(vm, a.this);
    let cap = promise::new_promise_capability_intrinsic(vm);
    let ret = match vm.get_method(iter, PropKey::Str(vm.names.return_)) {
        Ok(r) => r,
        Err(f) => return reject_or_propagate(vm, &cap, f),
    };
    let Some(ret) = ret else {
        let r = vm.create_iter_result(a.arg(0), true);
        promise::resolve_capability(vm, &cap, r)?;
        return Ok(Value::Object(cap.promise));
    };
    let args: Vec<Value> = a.args.iter().take(1).copied().collect();
    let result = match vm.call_value(ret, iter, &args) {
        Ok(r) => r,
        Err(f) => return reject_or_propagate(vm, &cap, f),
    };
    if !result.is_object() {
        let e = vm.make_error(ErrorType::TypeError, "iterator result is not an object");
        promise::reject_capability(vm, &cap, e)?;
        return Ok(Value::Object(cap.promise));
    }
    afs_continuation(vm, result, &cap, iter, false)
}

fn afs_throw(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (iter, _) = afs_parts(vm, a.this);
    let cap = promise::new_promise_capability_intrinsic(vm);
    let thr = match vm.get_method(iter, PropKey::Str(vm.names.throw)) {
        Ok(r) => r,
        Err(f) => return reject_or_propagate(vm, &cap, f),
    };
    let Some(thr) = thr else {
        // No throw method: close the sync iterator, then reject with a TypeError.
        if let Err(f) = vm.iterator_close(iter) {
            return reject_or_propagate(vm, &cap, f);
        }
        let e = vm.make_error(
            ErrorType::TypeError,
            "The iterator does not provide a 'throw' method",
        );
        promise::reject_capability(vm, &cap, e)?;
        return Ok(Value::Object(cap.promise));
    };
    let args: Vec<Value> = a.args.iter().take(1).copied().collect();
    let result = match vm.call_value(thr, iter, &args) {
        Ok(r) => r,
        Err(f) => return reject_or_propagate(vm, &cap, f),
    };
    if !result.is_object() {
        let e = vm.make_error(ErrorType::TypeError, "iterator result is not an object");
        promise::reject_capability(vm, &cap, e)?;
        return Ok(Value::Object(cap.promise));
    }
    afs_continuation(vm, result, &cap, iter, true)
}
