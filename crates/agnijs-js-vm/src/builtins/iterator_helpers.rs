//! Iterator helpers (ES2025 27.1.3): the `Iterator` constructor,
//! `Iterator.from`, the `Iterator.prototype` methods, and the
//! %IteratorHelperPrototype% / %WrapForValidIteratorPrototype% objects.
//!
//! A helper is a native "generator": `HelperState` holds the underlying
//! iterator record, the callback, the counter (or the remaining count for
//! `take`/`drop`), the inner iterator of a running `flatMap`, and the
//! done / executing flags that GeneratorValidate checks.

use super::iterator::HelperState;
use crate::convert::to_integer_or_infinity_f64;
use crate::object::{CallArgs, Class, ObjectData};
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, PropKey, Property, Value};
use crate::vm::{Fault, JsResult, Vm};
use agnijs_engine_api::RealmId;

const KIND_MAP: u8 = 0;
const KIND_FILTER: u8 = 1;
const KIND_TAKE: u8 = 2;
const KIND_DROP: u8 = 3;
const KIND_FLAT_MAP: u8 = 4;

pub fn install(vm: &mut Vm, realm: RealmId) {
    install_inner(vm, realm).expect("installing Iterator cannot throw");
}

fn install_inner(vm: &mut Vm, realm: RealmId) -> JsResult<()> {
    let iter_proto = vm.intrinsic(Intrinsic::IteratorPrototype);
    let ctor = super::install_ctor(
        vm,
        realm,
        "Iterator",
        0,
        iterator_constructor,
        iter_proto,
        Intrinsic::Iterator,
        Intrinsic::IteratorPrototype,
    );
    // `constructor` and @@toStringTag are accessors with setters that
    // ignore the prototype (SetterThatIgnoresPrototypeProperties).
    let ctor_key = PropKey::Str(vm.names.constructor);
    vm.delete_property(iter_proto, ctor_key)?;
    let g = vm.new_native("get constructor", 0, constructor_getter);
    let s = vm.new_native("set constructor", 1, constructor_setter);
    vm.heap.add_own_property(
        iter_proto,
        ctor_key,
        Property::accessor(Value::Object(g), Value::Object(s), Attrs::CONFIGURABLE_ONLY),
    );
    let tag_key = PropKey::Sym(vm.well_known.to_string_tag);
    let gname = vm.str("get [Symbol.toStringTag]");
    let g = vm.new_native_named(gname, 0, to_string_tag_getter);
    let sname = vm.str("set [Symbol.toStringTag]");
    let s = vm.new_native_named(sname, 1, to_string_tag_setter);
    vm.heap.add_own_property(
        iter_proto,
        tag_key,
        Property::accessor(Value::Object(g), Value::Object(s), Attrs::CONFIGURABLE_ONLY),
    );

    vm.method(ctor, "from", 1, iterator_from);

    vm.method(iter_proto, "map", 1, proto_map);
    vm.method(iter_proto, "filter", 1, proto_filter);
    vm.method(iter_proto, "take", 1, proto_take);
    vm.method(iter_proto, "drop", 1, proto_drop);
    vm.method(iter_proto, "flatMap", 1, proto_flat_map);
    vm.method(iter_proto, "reduce", 1, proto_reduce);
    vm.method(iter_proto, "toArray", 0, proto_to_array);
    vm.method(iter_proto, "forEach", 1, proto_for_each);
    vm.method(iter_proto, "some", 1, proto_some);
    vm.method(iter_proto, "every", 1, proto_every);
    vm.method(iter_proto, "find", 1, proto_find);

    let helper_proto = vm.intrinsic(Intrinsic::IteratorHelperPrototype);
    vm.method(helper_proto, "next", 0, helper_next);
    vm.method(helper_proto, "return", 0, helper_return);
    super::to_string_tag(vm, helper_proto, "Iterator Helper");

    let wrap_proto = vm.intrinsic(Intrinsic::WrapForValidIteratorPrototype);
    vm.method(wrap_proto, "next", 0, wrap_next);
    vm.method(wrap_proto, "return", 0, wrap_return);
    Ok(())
}

// ---- Iterator ---------------------------------------------------------------------------

fn iterator_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if !a.is_construct() || a.new_target == Value::Object(a.callee) {
        return vm.type_error("Iterator is an abstract class and cannot be constructed directly");
    }
    let o = vm.ordinary_create_from_constructor(
        a.new_target,
        Intrinsic::IteratorPrototype,
        Class::Ordinary,
    )?;
    Ok(Value::Object(o))
}

fn constructor_getter(vm: &mut Vm, _a: &CallArgs) -> JsResult<Value> {
    Ok(Value::Object(vm.intrinsic(Intrinsic::Iterator)))
}

fn constructor_setter(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let key = PropKey::Str(vm.names.constructor);
    setter_that_ignores_prototype_properties(vm, a.this, key, a.arg(0))
}

fn to_string_tag_getter(vm: &mut Vm, _a: &CallArgs) -> JsResult<Value> {
    Ok(vm.str("Iterator"))
}

fn to_string_tag_setter(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let key = PropKey::Sym(vm.well_known.to_string_tag);
    setter_that_ignores_prototype_properties(vm, a.this, key, a.arg(0))
}

/// SetterThatIgnoresPrototypeProperties(this, %Iterator.prototype%, p, v).
fn setter_that_ignores_prototype_properties(
    vm: &mut Vm,
    this: Value,
    key: PropKey,
    v: Value,
) -> JsResult<Value> {
    let Value::Object(o) = this else {
        return vm.type_error("Iterator.prototype setter called on a non-object");
    };
    if o == vm.intrinsic(Intrinsic::IteratorPrototype) {
        return vm.type_error("Cannot assign to a property of Iterator.prototype");
    }
    if vm.own_property(o, key)?.is_none() {
        vm.create_data_property_or_throw(o, key, v)?;
    } else if !vm.set_prop(o, key, v, this)? {
        return vm.type_error("Cannot assign to read only property of object");
    }
    Ok(Value::Undefined)
}

/// GetIteratorDirect(obj): the `next` method as a (iterator, next) record.
fn get_iterator_direct(vm: &mut Vm, o: Value) -> JsResult<(Value, Value)> {
    let next = vm.get_value(o, PropKey::Str(vm.names.next))?;
    Ok((o, next))
}

/// GetIteratorFlattenable(obj, primitiveHandling).
fn get_iterator_flattenable(vm: &mut Vm, obj: Value, iterate_strings: bool) -> JsResult<(Value, Value)> {
    if !obj.is_object() {
        if !(iterate_strings && obj.is_string()) {
            return vm.type_error("value is not an object");
        }
    }
    let method = vm.get_method(obj, PropKey::Sym(vm.well_known.iterator))?;
    let iterator = match method {
        None => obj,
        Some(m) => vm.call_value(m, obj, &[])?,
    };
    if !iterator.is_object() {
        return vm.type_error("iterator is not an object");
    }
    get_iterator_direct(vm, iterator)
}

fn iterator_from(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (iterator, next) = get_iterator_flattenable(vm, a.arg(0), true)?;
    let ctor = Value::Object(vm.intrinsic(Intrinsic::Iterator));
    if vm.ordinary_has_instance(ctor, iterator)? {
        return Ok(iterator);
    }
    let proto = vm.intrinsic(Intrinsic::WrapForValidIteratorPrototype);
    let wrapper = vm.alloc(ObjectData::new(
        Some(proto),
        Class::WrapForValidIterator(Box::new(crate::object::IteratorRecord { iterator, next })),
    ));
    Ok(Value::Object(wrapper))
}

fn wrap_parts(vm: &mut Vm, this: Value, method: &str) -> JsResult<(Value, Value)> {
    if let Value::Object(o) = this {
        if let Class::WrapForValidIterator(r) = &vm.heap.obj(o).class {
            return Ok((r.iterator, r.next));
        }
    }
    Err(vm.throw_type(&format!(
        "%WrapForValidIteratorPrototype%.{method} called on incompatible receiver"
    )))
}

fn wrap_next(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (iterator, next) = wrap_parts(vm, a.this, "next")?;
    vm.call_value(next, iterator, &[])
}

fn wrap_return(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (iterator, _) = wrap_parts(vm, a.this, "return")?;
    let ret = vm.get_method(iterator, PropKey::Str(vm.names.return_))?;
    match ret {
        None => Ok(vm.create_iter_result(Value::Undefined, true)),
        Some(r) => vm.call_value(r, iterator, &[]),
    }
}

// ---- shared prologue -----------------------------------------------------------------------

/// `this` must be an object; a non-callable argument closes `this` before
/// the TypeError is thrown (the argument-validation steps of every helper).
fn this_object(vm: &mut Vm, a: &CallArgs, method: &str) -> JsResult<Value> {
    if !a.this.is_object() {
        return Err(vm.throw_type(&format!(
            "Iterator.prototype.{method} called on a non-object"
        )));
    }
    Ok(a.this)
}

fn callable_or_close(vm: &mut Vm, this: Value, f: Value, what: &str) -> JsResult<()> {
    if vm.is_callable(f) {
        return Ok(());
    }
    let err = vm.throw_type(&format!("{what} is not a function"));
    Err(close_with_fault(vm, this, err))
}

/// IteratorClose(iterated, abrupt completion): the original fault wins.
fn close_with_fault(vm: &mut Vm, iter: Value, f: Fault) -> Fault {
    if matches!(f, Fault::Throw(_)) {
        vm.iterator_close_swallow(iter);
    }
    f
}

/// IfAbruptCloseIterator for a result.
fn close_on_err<T>(vm: &mut Vm, iter: Value, r: JsResult<T>) -> JsResult<T> {
    match r {
        Ok(v) => Ok(v),
        Err(f) => Err(close_with_fault(vm, iter, f)),
    }
}

fn new_helper(vm: &mut Vm, underlying: Value, next: Value, kind: u8, fns: Vec<Value>, counter: f64) -> Value {
    let proto = vm.intrinsic(Intrinsic::IteratorHelperPrototype);
    let state = HelperState {
        underlying,
        next,
        kind,
        fns,
        counter,
        done: false,
        executing: false,
        inner: None,
    };
    Value::Object(vm.alloc(ObjectData::new(
        Some(proto),
        Class::IteratorHelper(Box::new(state)),
    )))
}

/// The `limit` argument of take / drop: ToNumber, then a non-negative
/// integer or +∞; every failure closes the underlying iterator.
fn limit_arg(vm: &mut Vm, this: Value, limit: Value) -> JsResult<f64> {
    let n = vm.to_number(limit);
    let n = close_on_err(vm, this, n)?;
    if n.is_nan() {
        let err = vm.throw_range("limit must be a number");
        return Err(close_with_fault(vm, this, err));
    }
    let n = to_integer_or_infinity_f64(n);
    if n < 0.0 {
        let err = vm.throw_range("limit must be non-negative");
        return Err(close_with_fault(vm, this, err));
    }
    // test262 (2026) requires a finite limit to be a safe integer.
    if n != f64::INFINITY && n > 9007199254740991.0 {
        let err = vm.throw_range("limit must not exceed Number.MAX_SAFE_INTEGER");
        return Err(close_with_fault(vm, this, err));
    }
    Ok(n)
}

// ---- lazy helpers ----------------------------------------------------------------------------

fn proto_map(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let this = this_object(vm, a, "map")?;
    callable_or_close(vm, this, a.arg(0), "mapper")?;
    let (it, next) = get_iterator_direct(vm, this)?;
    Ok(new_helper(vm, it, next, KIND_MAP, vec![a.arg(0)], 0.0))
}

fn proto_filter(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let this = this_object(vm, a, "filter")?;
    callable_or_close(vm, this, a.arg(0), "predicate")?;
    let (it, next) = get_iterator_direct(vm, this)?;
    Ok(new_helper(vm, it, next, KIND_FILTER, vec![a.arg(0)], 0.0))
}

fn proto_take(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let this = this_object(vm, a, "take")?;
    let limit = limit_arg(vm, this, a.arg(0))?;
    let (it, next) = get_iterator_direct(vm, this)?;
    Ok(new_helper(vm, it, next, KIND_TAKE, Vec::new(), limit))
}

fn proto_drop(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let this = this_object(vm, a, "drop")?;
    let limit = limit_arg(vm, this, a.arg(0))?;
    let (it, next) = get_iterator_direct(vm, this)?;
    Ok(new_helper(vm, it, next, KIND_DROP, Vec::new(), limit))
}

fn proto_flat_map(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let this = this_object(vm, a, "flatMap")?;
    callable_or_close(vm, this, a.arg(0), "mapper")?;
    let (it, next) = get_iterator_direct(vm, this)?;
    Ok(new_helper(vm, it, next, KIND_FLAT_MAP, vec![a.arg(0)], 0.0))
}

fn helper_state(vm: &mut Vm, this: Value, method: &str) -> JsResult<ObjId> {
    if let Value::Object(o) = this {
        if matches!(vm.heap.obj(o).class, Class::IteratorHelper(_)) {
            return Ok(o);
        }
    }
    Err(vm.throw_type(&format!(
        "%IteratorHelperPrototype%.{method} called on incompatible receiver"
    )))
}

fn state(vm: &mut Vm, o: ObjId) -> &mut HelperState {
    match &mut vm.heap.obj_mut(o).class {
        Class::IteratorHelper(h) => h,
        _ => unreachable!("helper_state checked the class"),
    }
}

fn helper_next(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = helper_state(vm, a.this, "next")?;
    {
        let s = state(vm, o);
        if s.executing {
            return vm.type_error("Iterator helper is already running");
        }
        if s.done {
            return Ok(vm.create_iter_result(Value::Undefined, true));
        }
        s.executing = true;
    }
    let r = helper_step(vm, o);
    let s = state(vm, o);
    s.executing = false;
    match r {
        Ok(Some(v)) => Ok(vm.create_iter_result(v, false)),
        Ok(None) => {
            s.done = true;
            Ok(vm.create_iter_result(Value::Undefined, true))
        }
        Err(f) => {
            s.done = true;
            Err(f)
        }
    }
}

/// One resumption of the helper's closure: `Some(value)` to yield, `None`
/// when the closure returns.
fn helper_step(vm: &mut Vm, o: ObjId) -> JsResult<Option<Value>> {
    let (kind, under, next, f, counter, inner) = {
        let s = state(vm, o);
        (s.kind, s.underlying, s.next, s.fns.first().copied().unwrap_or(Value::Undefined), s.counter, s.inner)
    };
    match kind {
        KIND_MAP => {
            let Some(v) = vm.iterator_step_value(under, next)? else {
                return Ok(None);
            };
            let r = vm.call_value(f, Value::Undefined, &[v, Value::Number(counter)]);
            let mapped = close_on_err(vm, under, r)?;
            state(vm, o).counter = counter + 1.0;
            Ok(Some(mapped))
        }
        KIND_FILTER => {
            let mut counter = counter;
            loop {
                let Some(v) = vm.iterator_step_value(under, next)? else {
                    return Ok(None);
                };
                let r = vm.call_value(f, Value::Undefined, &[v, Value::Number(counter)]);
                let selected = close_on_err(vm, under, r)?;
                counter += 1.0;
                state(vm, o).counter = counter;
                if vm.to_boolean(selected) {
                    return Ok(Some(v));
                }
            }
        }
        KIND_TAKE => {
            let remaining = counter;
            if remaining == 0.0 {
                vm.iterator_close(under)?;
                return Ok(None);
            }
            if remaining != f64::INFINITY {
                state(vm, o).counter = remaining - 1.0;
            }
            match vm.iterator_step_value(under, next)? {
                Some(v) => Ok(Some(v)),
                None => Ok(None),
            }
        }
        KIND_DROP => {
            let mut remaining = counter;
            while remaining > 0.0 {
                if remaining != f64::INFINITY {
                    remaining -= 1.0;
                    state(vm, o).counter = remaining;
                }
                if vm.iterator_step_value(under, next)?.is_none() {
                    return Ok(None);
                }
            }
            match vm.iterator_step_value(under, next)? {
                Some(v) => Ok(Some(v)),
                None => Ok(None),
            }
        }
        KIND_FLAT_MAP => {
            let mut inner = inner;
            let mut counter = counter;
            loop {
                if let Some((iit, inext)) = inner {
                    let r = vm.iterator_step_value(iit, inext);
                    match close_on_err(vm, under, r)? {
                        Some(v) => return Ok(Some(v)),
                        None => {
                            inner = None;
                            state(vm, o).inner = None;
                        }
                    }
                    continue;
                }
                let Some(v) = vm.iterator_step_value(under, next)? else {
                    return Ok(None);
                };
                let r = vm.call_value(f, Value::Undefined, &[v, Value::Number(counter)]);
                let mapped = close_on_err(vm, under, r)?;
                let r = get_iterator_flattenable(vm, mapped, false);
                let (iit, inext) = close_on_err(vm, under, r)?;
                counter += 1.0;
                let s = state(vm, o);
                s.counter = counter;
                s.inner = Some((iit, inext));
                inner = Some((iit, inext));
            }
        }
        _ => unreachable!("unknown iterator helper kind"),
    }
}

fn helper_return(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = helper_state(vm, a.this, "return")?;
    let (executing, done, under, inner) = {
        let s = state(vm, o);
        (s.executing, s.done, s.underlying, s.inner)
    };
    if executing {
        return vm.type_error("Iterator helper is already running");
    }
    if done {
        return Ok(vm.create_iter_result(Value::Undefined, true));
    }
    let s = state(vm, o);
    s.done = true;
    s.inner = None;
    // A return while an inner iterator (flatMap) is live closes it first;
    // its error, if any, still closes the outer iterator before winning.
    if let Some((iit, _)) = inner {
        if let Err(f) = vm.iterator_close(iit) {
            return Err(close_with_fault(vm, under, f));
        }
    }
    vm.iterator_close(under)?;
    Ok(vm.create_iter_result(Value::Undefined, true))
}

// ---- eager helpers ----------------------------------------------------------------------------

fn proto_reduce(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let this = this_object(vm, a, "reduce")?;
    let reducer = a.arg(0);
    callable_or_close(vm, this, reducer, "reducer")?;
    let (it, next) = get_iterator_direct(vm, this)?;
    let (mut acc, mut counter) = if a.len() < 2 {
        match vm.iterator_step_value(it, next)? {
            Some(v) => (v, 1.0),
            None => return vm.type_error("Reduce of empty iterator with no initial value"),
        }
    } else {
        (a.arg(1), 0.0)
    };
    loop {
        let Some(v) = vm.iterator_step_value(it, next)? else {
            return Ok(acc);
        };
        let r = vm.call_value(reducer, Value::Undefined, &[acc, v, Value::Number(counter)]);
        acc = close_on_err(vm, it, r)?;
        counter += 1.0;
    }
}

fn proto_to_array(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let this = this_object(vm, a, "toArray")?;
    let (it, next) = get_iterator_direct(vm, this)?;
    let mut items = Vec::new();
    while let Some(v) = vm.iterator_step_value(it, next)? {
        items.push(v);
    }
    Ok(Value::Object(vm.new_array(items)))
}

fn proto_for_each(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let this = this_object(vm, a, "forEach")?;
    let f = a.arg(0);
    callable_or_close(vm, this, f, "callback")?;
    let (it, next) = get_iterator_direct(vm, this)?;
    let mut counter = 0.0;
    while let Some(v) = vm.iterator_step_value(it, next)? {
        let r = vm.call_value(f, Value::Undefined, &[v, Value::Number(counter)]);
        close_on_err(vm, it, r)?;
        counter += 1.0;
    }
    Ok(Value::Undefined)
}

/// some / every / find share one loop: `stop_when` is the predicate result
/// that ends the iteration early (closing the iterator); `found` says what
/// an early exit returns.
fn search(vm: &mut Vm, a: &CallArgs, method: &str, stop_when: bool, found: fn(Value) -> Value, exhausted: Value) -> JsResult<Value> {
    let this = this_object(vm, a, method)?;
    let f = a.arg(0);
    callable_or_close(vm, this, f, "predicate")?;
    let (it, next) = get_iterator_direct(vm, this)?;
    let mut counter = 0.0;
    while let Some(v) = vm.iterator_step_value(it, next)? {
        let r = vm.call_value(f, Value::Undefined, &[v, Value::Number(counter)]);
        let r = close_on_err(vm, it, r)?;
        if vm.to_boolean(r) == stop_when {
            vm.iterator_close(it)?;
            return Ok(found(v));
        }
        counter += 1.0;
    }
    Ok(exhausted)
}

fn proto_some(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    search(vm, a, "some", true, |_| Value::Bool(true), Value::Bool(false))
}

fn proto_every(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    search(vm, a, "every", false, |_| Value::Bool(false), Value::Bool(true))
}

fn proto_find(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    search(vm, a, "find", true, |v| v, Value::Undefined)
}
