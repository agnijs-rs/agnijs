//! Iterator helpers (ES2025 27.1.3): the `Iterator` constructor,
//! `Iterator.from`, the `Iterator.prototype` methods, and the
//! %IteratorHelperPrototype% / %WrapForValidIteratorPrototype% objects.
//!
//! A helper is a native "generator": `HelperState` holds the underlying
//! iterator record, the callback, the counter (or the remaining count for
//! `take`/`drop`), the inner iterator of a running `flatMap`, and the
//! done / executing flags that GeneratorValidate checks.

use super::iterator::{HelperState, ZipMode, ZipState};
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
const KIND_CONCAT: u8 = 5;
const KIND_CHUNKS: u8 = 6;
const KIND_WINDOWS: u8 = 7;
const KIND_WINDOWS_PARTIAL: u8 = 8;
const KIND_ZIP: u8 = 9;

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

    vm.method(ctor, "concat", 0, iterator_concat);
    vm.method(ctor, "from", 1, iterator_from);
    vm.method(ctor, "zip", 1, iterator_zip);
    vm.method(ctor, "zipKeyed", 1, iterator_zip_keyed);

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
    vm.method(iter_proto, "includes", 1, proto_includes);
    vm.method(iter_proto, "chunks", 1, proto_chunks);
    vm.method(iter_proto, "windows", 1, proto_windows);

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
        zip: None,
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
                vm.poll()?;
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
                vm.poll()?;
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
                vm.poll()?;
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
        KIND_CONCAT => concat_step(vm, o),
        KIND_CHUNKS => {
            let size = counter as usize;
            loop {
                vm.poll()?;
                match vm.iterator_step_value(under, next)? {
                    Some(v) => {
                        let s = state(vm, o);
                        s.fns.push(v);
                        if s.fns.len() == size {
                            let chunk = std::mem::take(&mut s.fns);
                            return Ok(Some(Value::Object(vm.new_array(chunk))));
                        }
                    }
                    None => {
                        let chunk = std::mem::take(&mut state(vm, o).fns);
                        if chunk.is_empty() {
                            return Ok(None);
                        }
                        return Ok(Some(Value::Object(vm.new_array(chunk))));
                    }
                }
            }
        }
        KIND_WINDOWS | KIND_WINDOWS_PARTIAL => {
            let size = counter as usize;
            loop {
                vm.poll()?;
                match vm.iterator_step_value(under, next)? {
                    Some(v) => {
                        let s = state(vm, o);
                        if s.fns.len() == size {
                            s.fns.remove(0);
                        }
                        s.fns.push(v);
                        if s.fns.len() == size {
                            let window = s.fns.clone();
                            return Ok(Some(Value::Object(vm.new_array(window))));
                        }
                    }
                    None => {
                        // The buffer stays full once the first window is
                        // yielded, so a short buffer was never yielded.
                        let buf = std::mem::take(&mut state(vm, o).fns);
                        if kind == KIND_WINDOWS_PARTIAL && !buf.is_empty() && buf.len() < size {
                            return Ok(Some(Value::Object(vm.new_array(buf))));
                        }
                        return Ok(None);
                    }
                }
            }
        }
        KIND_ZIP => zip_step(vm, o),
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
    let kind = state(vm, o).kind;
    if kind == KIND_ZIP {
        let z = state(vm, o).zip.take();
        state(vm, o).done = true;
        if let Some(z) = z {
            // From suspended-yield the helper is executing while it closes.
            state(vm, o).executing = z.started;
            let r = close_all(vm, &open_iters(&z), Ok(()));
            state(vm, o).executing = false;
            r?;
        }
        return Ok(vm.create_iter_result(Value::Undefined, true));
    }
    let s = state(vm, o);
    s.done = true;
    s.inner = None;
    if kind == KIND_CONCAT {
        // Only the iterator being drained is open; none before the start.
        if let Some((iit, _)) = inner {
            state(vm, o).executing = true;
            let r = vm.iterator_close(iit);
            state(vm, o).executing = false;
            r?;
        }
        return Ok(vm.create_iter_result(Value::Undefined, true));
    }
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
        vm.poll()?;
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
        vm.poll()?;
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
        vm.poll()?;
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
        vm.poll()?;
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

// ---- ES2026: includes, chunks, windows (iterator includes, iterator chunking) ---------------

/// A size argument of `chunks` / `windows`: an integral Number (no
/// coercion) in 1..=2^32-1. A failure closes `this`.
fn size_arg(vm: &mut Vm, this: Value, v: Value, what: &str) -> JsResult<f64> {
    let n = match v {
        Value::Number(n) if n.is_finite() && n.trunc() == n => n,
        _ => {
            let err = vm.throw_type(&format!("{what} must be an integral Number"));
            return Err(close_with_fault(vm, this, err));
        }
    };
    if !(1.0..=4294967295.0).contains(&n) {
        let err = vm.throw_range(&format!("{what} must be between 1 and 2^32 - 1"));
        return Err(close_with_fault(vm, this, err));
    }
    Ok(n)
}

fn proto_chunks(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let this = this_object(vm, a, "chunks")?;
    let size = size_arg(vm, this, a.arg(0), "chunkSize")?;
    let (it, next) = get_iterator_direct(vm, this)?;
    Ok(new_helper(vm, it, next, KIND_CHUNKS, Vec::new(), size))
}

fn proto_windows(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let this = this_object(vm, a, "windows")?;
    let size = size_arg(vm, this, a.arg(0), "windowSize")?;
    let kind = match a.arg(1) {
        Value::Undefined => KIND_WINDOWS,
        Value::String(s) if vm.heap.str_eq_ascii(s, "only-full") => KIND_WINDOWS,
        Value::String(s) if vm.heap.str_eq_ascii(s, "allow-partial") => KIND_WINDOWS_PARTIAL,
        _ => {
            let err = vm.throw_type("undersized must be \"only-full\" or \"allow-partial\"");
            return Err(close_with_fault(vm, this, err));
        }
    };
    let (it, next) = get_iterator_direct(vm, this)?;
    Ok(new_helper(vm, it, next, kind, Vec::new(), size))
}

/// Iterator.prototype.includes(searchElement, skippedElements): the
/// number of elements to skip is an integral Number (no coercion) or +∞.
fn proto_includes(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let this = this_object(vm, a, "includes")?;
    let to_skip = match a.arg(1) {
        Value::Undefined => 0.0,
        Value::Number(n) if n == f64::INFINITY => n,
        Value::Number(n) if n == f64::NEG_INFINITY => {
            let err = vm.throw_range("skippedElements must not be negative");
            return Err(close_with_fault(vm, this, err));
        }
        Value::Number(n) if n.is_finite() && n.trunc() == n => {
            if n < 0.0 {
                let err = vm.throw_range("skippedElements must not be negative");
                return Err(close_with_fault(vm, this, err));
            }
            if n > 9007199254740991.0 {
                let err = vm.throw_range("skippedElements must not exceed Number.MAX_SAFE_INTEGER");
                return Err(close_with_fault(vm, this, err));
            }
            n
        }
        _ => {
            let err = vm.throw_type("skippedElements must be an integral Number");
            return Err(close_with_fault(vm, this, err));
        }
    };
    let search = a.arg(0);
    let (it, next) = get_iterator_direct(vm, this)?;
    let mut skipped = 0.0;
    while let Some(v) = vm.iterator_step_value(it, next)? {
        vm.poll()?;
        if skipped < to_skip {
            skipped += 1.0;
        } else if vm.same_value_zero(v, search) {
            vm.iterator_close(it)?;
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}

// ---- ES2026: Iterator.concat (iterator sequencing) ------------------------------------------

/// Iterator.concat(...items): every item is an object with a callable
/// @@iterator, read once up front; the helper opens each in turn.
fn iterator_concat(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let mut pairs = Vec::with_capacity(a.len() * 2);
    for &item in a.args {
        if !item.is_object() {
            return vm.type_error("Iterator.concat: every argument must be an object");
        }
        let Some(method) = vm.get_method(item, PropKey::Sym(vm.well_known.iterator))? else {
            return vm.type_error("Iterator.concat: argument is not iterable");
        };
        pairs.push(item);
        pairs.push(method);
    }
    Ok(new_helper(vm, Value::Undefined, Value::Undefined, KIND_CONCAT, pairs, 0.0))
}

fn concat_step(vm: &mut Vm, o: ObjId) -> JsResult<Option<Value>> {
    loop {
        vm.poll()?;
        let (inner, index, item, method) = {
            let s = state(vm, o);
            let i = s.counter as usize;
            let pair = (s.fns.get(2 * i).copied(), s.fns.get(2 * i + 1).copied());
            (s.inner, i, pair.0, pair.1)
        };
        if let Some((iit, inext)) = inner {
            match vm.iterator_step_value(iit, inext)? {
                Some(v) => return Ok(Some(v)),
                None => {
                    state(vm, o).inner = None;
                    continue;
                }
            }
        }
        let (Some(item), Some(method)) = (item, method) else {
            return Ok(None);
        };
        state(vm, o).counter = (index + 1) as f64;
        let iter = vm.call_value(method, item, &[])?;
        if !iter.is_object() {
            return vm.type_error("Iterator.concat: the @@iterator method returned a non-object");
        }
        let rec = get_iterator_direct(vm, iter)?;
        state(vm, o).inner = Some(rec);
    }
}

// ---- ES2026: Iterator.zip / Iterator.zipKeyed (joint iteration) ----------------------------

/// IteratorCloseAll(iters, completion): close in reverse order; the first
/// throw wins and later `return` errors are swallowed.
fn close_all(vm: &mut Vm, iters: &[Value], completion: JsResult<()>) -> JsResult<()> {
    let mut completion = completion;
    for &it in iters.iter().rev() {
        completion = match completion {
            Ok(()) => vm.iterator_close(it),
            Err(Fault::Throw(e)) => {
                vm.iterator_close_swallow(it);
                Err(Fault::Throw(e))
            }
            Err(f) => return Err(f),
        };
    }
    completion
}

fn open_iters(z: &ZipState) -> Vec<Value> {
    z.iters
        .iter()
        .zip(&z.open)
        .filter_map(|(r, &open)| if open { r.map(|(it, _)| it) } else { None })
        .collect()
}

/// Close `iters` with a throw completion and return the fault.
fn close_all_fault(vm: &mut Vm, iters: &[(Value, Value)], f: Fault) -> Fault {
    let list: Vec<Value> = iters.iter().map(|r| r.0).collect();
    match close_all(vm, &list, Err(f)) {
        Err(f) => f,
        Ok(()) => unreachable!("closing with a throw completion stays abrupt"),
    }
}

/// The options of zip / zipKeyed: the mode, and for `longest` the padding
/// option (undefined or an object).
fn zip_options(vm: &mut Vm, options: Value) -> JsResult<(ZipMode, Value)> {
    let opts = match options {
        Value::Undefined => return Ok((ZipMode::Shortest, Value::Undefined)),
        Value::Object(_) => options,
        _ => return vm.type_error("options must be an object"),
    };
    let key = PropKey::Str(vm.atom("mode"));
    let mode = match vm.get_value(opts, key)? {
        Value::Undefined => ZipMode::Shortest,
        Value::String(s) if vm.heap.str_eq_ascii(s, "shortest") => ZipMode::Shortest,
        Value::String(s) if vm.heap.str_eq_ascii(s, "longest") => ZipMode::Longest,
        Value::String(s) if vm.heap.str_eq_ascii(s, "strict") => ZipMode::Strict,
        _ => return vm.type_error("mode must be \"shortest\", \"longest\" or \"strict\""),
    };
    let mut padding = Value::Undefined;
    if mode == ZipMode::Longest {
        let key = PropKey::Str(vm.atom("padding"));
        padding = vm.get_value(opts, key)?;
        if !padding.is_undefined() && !padding.is_object() {
            return vm.type_error("padding must be an object");
        }
    }
    Ok((mode, padding))
}

fn new_zip(vm: &mut Vm, iters: Vec<(Value, Value)>, mode: ZipMode, padding: Vec<Value>, keys: Option<Vec<Value>>) -> Value {
    let h = new_helper(vm, Value::Undefined, Value::Undefined, KIND_ZIP, Vec::new(), 0.0);
    let Value::Object(o) = h else { unreachable!("new_helper returns an object") };
    let n = iters.len();
    state(vm, o).zip = Some(Box::new(ZipState {
        iters: iters.into_iter().map(Some).collect(),
        open: vec![true; n],
        padding,
        keys,
        mode,
        started: false,
    }));
    h
}

fn iterator_zip(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let iterables = a.arg(0);
    if !iterables.is_object() {
        return vm.type_error("Iterator.zip: iterables must be an object");
    }
    let (mode, padding_opt) = zip_options(vm, a.arg(1))?;
    let (input, input_next) = vm.get_iterator(iterables, false)?;
    let mut iters: Vec<(Value, Value)> = Vec::new();
    loop {
        let next = match vm.iterator_step_value(input, input_next) {
            Ok(n) => n,
            Err(f) => return Err(close_all_fault(vm, &iters, f)),
        };
        let Some(v) = next else { break };
        match get_iterator_flattenable(vm, v, false) {
            Ok(rec) => iters.push(rec),
            Err(f) => {
                let mut all = vec![(input, input_next)];
                all.extend_from_slice(&iters);
                return Err(close_all_fault(vm, &all, f));
            }
        }
    }
    let n = iters.len();
    let mut padding = Vec::with_capacity(if mode == ZipMode::Longest { n } else { 0 });
    if mode == ZipMode::Longest {
        if padding_opt.is_undefined() {
            padding.resize(n, Value::Undefined);
        } else {
            let (pit, pnext) = match vm.get_iterator(padding_opt, false) {
                Ok(r) => r,
                Err(f) => return Err(close_all_fault(vm, &iters, f)),
            };
            let mut using = true;
            for _ in 0..n {
                if using {
                    match vm.iterator_step_value(pit, pnext) {
                        Ok(Some(v)) => padding.push(v),
                        Ok(None) => using = false,
                        Err(f) => return Err(close_all_fault(vm, &iters, f)),
                    }
                }
                if !using {
                    padding.push(Value::Undefined);
                }
            }
            if using {
                if let Err(f) = vm.iterator_close(pit) {
                    return Err(close_all_fault(vm, &iters, f));
                }
            }
        }
    }
    Ok(new_zip(vm, iters, mode, padding, None))
}

fn iterator_zip_keyed(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Value::Object(obj) = a.arg(0) else {
        return vm.type_error("Iterator.zipKeyed: iterables must be an object");
    };
    let (mode, padding_opt) = zip_options(vm, a.arg(1))?;
    let mut iters: Vec<(Value, Value)> = Vec::new();
    let mut keys: Vec<Value> = Vec::new();
    let all_keys = vm.own_property_keys(obj)?;
    for key in all_keys {
        let desc = match vm.own_property(obj, key) {
            Ok(d) => d,
            Err(f) => return Err(close_all_fault(vm, &iters, f)),
        };
        let Some(desc) = desc else { continue };
        if !desc.attrs.enumerable() {
            continue;
        }
        let value = match vm.get(obj, key) {
            Ok(v) => v,
            Err(f) => return Err(close_all_fault(vm, &iters, f)),
        };
        if value.is_undefined() {
            continue;
        }
        match get_iterator_flattenable(vm, value, false) {
            Ok(rec) => {
                let k = vm.key_to_value(key);
                keys.push(k);
                iters.push(rec);
            }
            Err(f) => return Err(close_all_fault(vm, &iters, f)),
        }
    }
    let n = iters.len();
    let mut padding = Vec::new();
    if mode == ZipMode::Longest {
        if padding_opt.is_undefined() {
            padding.resize(n, Value::Undefined);
        } else {
            for &k in &keys {
                let r = vm.to_property_key(k).and_then(|pk| vm.get_value(padding_opt, pk));
                match r {
                    Ok(v) => padding.push(v),
                    Err(f) => return Err(close_all_fault(vm, &iters, f)),
                }
            }
        }
    }
    Ok(new_zip(vm, iters, mode, padding, Some(keys)))
}

/// One step of IteratorZip's closure. The state is taken out of the helper
/// for the duration (the executing flag keeps re-entry out) and put back.
fn zip_step(vm: &mut Vm, o: ObjId) -> JsResult<Option<Value>> {
    let Some(mut z) = state(vm, o).zip.take() else {
        return Ok(None);
    };
    z.started = true;
    let r = zip_step_inner(vm, &mut z);
    state(vm, o).zip = Some(z);
    r
}

fn zip_step_inner(vm: &mut Vm, z: &mut ZipState) -> JsResult<Option<Value>> {
    let n = z.iters.len();
    if n == 0 {
        return Ok(None);
    }
    let mut results = Vec::with_capacity(n);
    for i in 0..n {
        let Some((it, next)) = z.iters[i] else {
            results.push(z.padding[i]);
            continue;
        };
        let r = match vm.iterator_step_value(it, next) {
            Ok(r) => r,
            Err(f) => {
                z.open[i] = false;
                close_all(vm, &open_iters(z), Err(f))?;
                unreachable!("closing with a throw completion stays abrupt");
            }
        };
        match r {
            Some(v) => results.push(v),
            None => {
                z.open[i] = false;
                match z.mode {
                    ZipMode::Shortest => {
                        close_all(vm, &open_iters(z), Ok(()))?;
                        return Ok(None);
                    }
                    ZipMode::Strict => {
                        if i != 0 {
                            let e = vm.throw_type("Iterator.zip: iterators of different lengths in strict mode");
                            close_all(vm, &open_iters(z), Err(e))?;
                            unreachable!("closing with a throw completion stays abrupt");
                        }
                        for k in 1..n {
                            let Some((kit, knext)) = z.iters[k] else { continue };
                            match iterator_step(vm, kit, knext) {
                                Err(f) => {
                                    z.open[k] = false;
                                    close_all(vm, &open_iters(z), Err(f))?;
                                    unreachable!("closing with a throw completion stays abrupt");
                                }
                                Ok(true) => z.open[k] = false,
                                Ok(false) => {
                                    let e = vm.throw_type("Iterator.zip: iterators of different lengths in strict mode");
                                    close_all(vm, &open_iters(z), Err(e))?;
                                    unreachable!("closing with a throw completion stays abrupt");
                                }
                            }
                        }
                        return Ok(None);
                    }
                    ZipMode::Longest => {
                        if !z.open.iter().any(|&b| b) {
                            return Ok(None);
                        }
                        z.iters[i] = None;
                        results.push(z.padding[i]);
                    }
                }
            }
        }
    }
    let out = match &z.keys {
        None => Value::Object(vm.new_array(results)),
        Some(keys) => {
            let obj = vm.new_object_with_proto(None);
            for (k, v) in keys.clone().into_iter().zip(results) {
                let pk = vm.to_property_key(k)?;
                vm.create_data_property_or_throw(obj, pk, v)?;
            }
            Value::Object(obj)
        }
    };
    Ok(Some(out))
}

/// IteratorStep: `true` when the iterator is done (its value is not read).
fn iterator_step(vm: &mut Vm, it: Value, next: Value) -> JsResult<bool> {
    let r = vm.call_value(next, it, &[])?;
    let Value::Object(ro) = r else {
        return vm.type_error("Iterator result is not an object");
    };
    let done = vm.get(ro, PropKey::Str(vm.names.done))?;
    Ok(vm.to_boolean(done))
}
