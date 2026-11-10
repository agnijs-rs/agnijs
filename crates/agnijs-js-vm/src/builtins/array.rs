//! Array constructor and Array.prototype.

use crate::builtins::iterator::create_array_iterator;
use crate::convert::to_integer_or_infinity_f64;
use crate::object::{ArrayData, CallArgs, Class, IterKind, ObjectData};
use crate::realm::Intrinsic;
use crate::string::StrBuilder;
use crate::value::{Attrs, ObjId, PropKey, Value};
use crate::vm::{Fault, JsResult, Vm};
use agnijs_engine_api::RealmId;
use std::cmp::Ordering;

const MAX_SAFE: f64 = 9007199254740991.0;

pub fn install(vm: &mut Vm, realm: RealmId) {
    let object_proto = vm.intrinsic(Intrinsic::ObjectPrototype);
    let proto = vm.alloc(ObjectData::new(
        Some(object_proto),
        Class::Array(ArrayData {
            length: 0,
            length_writable: true,
        }),
    ));
    let ctor = super::install_ctor(
        vm,
        realm,
        "Array",
        1,
        array_constructor,
        proto,
        Intrinsic::Array,
        Intrinsic::ArrayPrototype,
    );
    vm.method(ctor, "from", 1, array_from);
    vm.method(ctor, "isArray", 1, array_is_array);
    vm.method(ctor, "of", 0, array_of);
    super::species_accessor(vm, ctor);

    vm.method(proto, "at", 1, proto_at);
    vm.method(proto, "concat", 1, proto_concat);
    vm.method(proto, "copyWithin", 2, proto_copy_within);
    vm.method(proto, "entries", 0, proto_entries);
    vm.method(proto, "every", 1, proto_every);
    vm.method(proto, "fill", 1, proto_fill);
    vm.method(proto, "filter", 1, proto_filter);
    vm.method(proto, "find", 1, proto_find);
    vm.method(proto, "findIndex", 1, proto_find_index);
    vm.method(proto, "findLast", 1, proto_find_last);
    vm.method(proto, "findLastIndex", 1, proto_find_last_index);
    vm.method(proto, "flat", 0, proto_flat);
    vm.method(proto, "flatMap", 1, proto_flat_map);
    vm.method(proto, "forEach", 1, proto_for_each);
    vm.method(proto, "includes", 1, proto_includes);
    vm.method(proto, "indexOf", 1, proto_index_of);
    vm.method(proto, "join", 1, proto_join);
    vm.method(proto, "keys", 0, proto_keys);
    vm.method(proto, "lastIndexOf", 1, proto_last_index_of);
    vm.method(proto, "map", 1, proto_map);
    vm.method(proto, "pop", 0, proto_pop);
    vm.method(proto, "push", 1, proto_push);
    vm.method(proto, "reduce", 1, proto_reduce);
    vm.method(proto, "reduceRight", 1, proto_reduce_right);
    vm.method(proto, "reverse", 0, proto_reverse);
    vm.method(proto, "shift", 0, proto_shift);
    vm.method(proto, "slice", 2, proto_slice);
    vm.method(proto, "some", 1, proto_some);
    vm.method(proto, "sort", 1, proto_sort);
    vm.method(proto, "splice", 2, proto_splice);
    vm.method(proto, "toLocaleString", 0, proto_to_locale_string);
    vm.method(proto, "toReversed", 0, proto_to_reversed);
    vm.method(proto, "toSorted", 1, proto_to_sorted);
    vm.method(proto, "toSpliced", 2, proto_to_spliced);
    vm.method(proto, "toString", 0, proto_to_string);
    vm.method(proto, "unshift", 1, proto_unshift);
    let values = vm.method(proto, "values", 0, proto_values);
    vm.realm_mut()
        .set_intrinsic(Intrinsic::ArrayPrototypeValues, values);
    vm.method(proto, "with", 2, proto_with);
    let iter = vm.well_known.iterator;
    vm.define_raw(
        proto,
        PropKey::Sym(iter),
        Value::Object(values),
        Attrs::METHOD,
    );

    let unscopables = vm.new_object_with_proto(None);
    for name in [
        "at",
        "copyWithin",
        "entries",
        "fill",
        "find",
        "findIndex",
        "findLast",
        "findLastIndex",
        "flat",
        "flatMap",
        "includes",
        "keys",
        "toReversed",
        "toSorted",
        "toSpliced",
        "values",
    ] {
        vm.define_raw_str(unscopables, name, Value::Bool(true), Attrs::ALL);
    }
    let us = vm.well_known.unscopables;
    vm.define_raw(
        proto,
        PropKey::Sym(us),
        Value::Object(unscopables),
        Attrs::CONFIGURABLE_ONLY,
    );
}

// ---- helpers -------------------------------------------------------------------------

fn this_obj(vm: &mut Vm, a: &CallArgs) -> JsResult<(ObjId, f64)> {
    let o = vm.to_object(a.this)?;
    let len = vm.length_of_array_like(o)?;
    Ok((o, len))
}

fn callback(vm: &mut Vm, v: Value, name: &str) -> JsResult<Value> {
    if !vm.is_callable(v) {
        let d = vm.describe_for_error(v);
        return vm.type_error(&format!("{d} is not a function (Array.prototype.{name})"));
    }
    Ok(v)
}

/// Resolve a relative index argument against `len` (`undefined` → default).
fn relative_index(vm: &mut Vm, v: Value, len: f64, default: f64) -> JsResult<f64> {
    if v.is_undefined() {
        return Ok(default);
    }
    let r = vm.to_integer_or_infinity(v)?;
    Ok(if r < 0.0 {
        (len + r).max(0.0)
    } else {
        r.min(len)
    })
}

fn set_length(vm: &mut Vm, o: ObjId, len: f64) -> JsResult<()> {
    let key = PropKey::Str(vm.names.length);
    vm.set(o, key, Value::Number(len), true)
}

/// ArraySpeciesCreate(originalArray, length).
pub fn array_species_create(vm: &mut Vm, original: ObjId, length: f64) -> JsResult<ObjId> {
    if !vm.is_array(Value::Object(original))? {
        return vm.array_create(length, None);
    }
    let ck = PropKey::Str(vm.names.constructor);
    let mut c = vm.get(original, ck)?;
    if vm.is_constructor(c) {
        let Value::Object(co) = c else { unreachable!() };
        let c_realm = vm.function_realm(co)?;
        if c_realm != vm.current_realm && co == vm.intrinsic_in(c_realm, Intrinsic::Array) {
            c = Value::Undefined;
        }
    }
    if let Value::Object(co) = c {
        let sk = PropKey::Sym(vm.well_known.species);
        c = vm.get(co, sk)?;
        if c.is_null() {
            c = Value::Undefined;
        }
    }
    if c.is_undefined() {
        return vm.array_create(length, None);
    }
    if !vm.is_constructor(c) {
        return vm.type_error("Array species constructor is not a constructor");
    }
    let r = vm.construct_value(c, &[Value::Number(length)], c)?;
    match r {
        Value::Object(o) => Ok(o),
        _ => vm.type_error("species constructor returned a non-object"),
    }
}

/// IsConcatSpreadable(O).
fn is_concat_spreadable(vm: &mut Vm, v: Value) -> JsResult<bool> {
    let Value::Object(o) = v else {
        return Ok(false);
    };
    let key = PropKey::Sym(vm.well_known.is_concat_spreadable);
    let s = vm.get(o, key)?;
    if !s.is_undefined() {
        return Ok(vm.to_boolean(s));
    }
    vm.is_array(v)
}

// ---- constructor and statics -----------------------------------------------------------

fn array_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let new_target = if a.new_target.is_undefined() {
        Value::Object(a.callee)
    } else {
        a.new_target
    };
    let proto = vm.get_prototype_from_constructor(new_target, Intrinsic::ArrayPrototype)?;
    match a.len() {
        0 => Ok(Value::Object(vm.array_create(0.0, Some(proto))?)),
        1 => {
            let len = a.arg(0);
            let arr = vm.array_create(0.0, Some(proto))?;
            match len {
                Value::Number(n) => {
                    let int_len = crate::convert::f64_to_uint32(n);
                    if int_len as f64 != n {
                        return vm.range_error("Invalid array length");
                    }
                    set_length(vm, arr, int_len as f64)?;
                }
                _ => {
                    vm.create_data_property_or_throw(arr, PropKey::Index(0), len)?;
                }
            }
            Ok(Value::Object(arr))
        }
        n => {
            let arr = vm.array_create(n as f64, Some(proto))?;
            for (i, &v) in a.args.iter().enumerate() {
                vm.create_data_property_or_throw(arr, PropKey::Index(i as u32), v)?;
            }
            Ok(Value::Object(arr))
        }
    }
}

fn array_is_array(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    Ok(Value::Bool(vm.is_array(a.arg(0))?))
}

fn array_of(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let len = a.len() as f64;
    let c = a.this;
    let arr = if vm.is_constructor(c) {
        match vm.construct_value(c, &[Value::Number(len)], c)? {
            Value::Object(o) => o,
            _ => return vm.type_error("constructor returned a non-object"),
        }
    } else {
        vm.array_create(len, None)?
    };
    for (i, &v) in a.args.iter().enumerate() {
        let k = vm.number_key(i as f64);
        vm.create_data_property_or_throw(arr, k, v)?;
    }
    set_length(vm, arr, len)?;
    Ok(Value::Object(arr))
}

fn array_from(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let c = a.this;
    let items = a.arg(0);
    let mapfn = a.arg(1);
    let this_arg = a.arg(2);
    let mapping = !mapfn.is_undefined();
    if mapping && !vm.is_callable(mapfn) {
        return vm.type_error("Array.from: mapper is not a function");
    }
    let iter_key = PropKey::Sym(vm.well_known.iterator);
    let using_iterator = vm.get_method(items, iter_key)?;
    if let Some(method) = using_iterator {
        let arr = if vm.is_constructor(c) {
            match vm.construct_value(c, &[], c)? {
                Value::Object(o) => o,
                _ => return vm.type_error("constructor returned a non-object"),
            }
        } else {
            vm.array_create(0.0, None)?
        };
        let mark = vm.root_mark();
        vm.root(Value::Object(arr));
        let r = array_from_iterator(vm, arr, items, method, mapping, mapfn, this_arg);
        vm.pop_roots(mark);
        r?;
        return Ok(Value::Object(arr));
    }
    let array_like = vm.to_object(items)?;
    let len = vm.length_of_array_like(array_like)?;
    let arr = if vm.is_constructor(c) {
        match vm.construct_value(c, &[Value::Number(len)], c)? {
            Value::Object(o) => o,
            _ => return vm.type_error("constructor returned a non-object"),
        }
    } else {
        vm.array_create(len, None)?
    };
    let mut k = 0.0;
    while k < len {
        let key = vm.number_key(k);
        let v = vm.get(array_like, key)?;
        let v = if mapping {
            vm.call_value(mapfn, this_arg, &[v, Value::Number(k)])?
        } else {
            v
        };
        vm.create_data_property_or_throw(arr, key, v)?;
        k += 1.0;
    }
    set_length(vm, arr, len)?;
    Ok(Value::Object(arr))
}

fn array_from_iterator(
    vm: &mut Vm,
    arr: ObjId,
    items: Value,
    method: Value,
    mapping: bool,
    mapfn: Value,
    this_arg: Value,
) -> JsResult<()> {
    let iter = vm.call_value(method, items, &[])?;
    if !iter.is_object() {
        return vm.type_error("Result of the Symbol.iterator method is not an object");
    }
    let next_key = PropKey::Str(vm.names.next);
    let next = vm.get_value(iter, next_key)?;
    let mut k = 0.0;
    loop {
        if k >= MAX_SAFE {
            let e = vm.make_error(
                crate::vm::ErrorType::TypeError,
                "Array.from: too many elements",
            );
            vm.iterator_close_swallow(iter);
            return Err(Fault::Throw(e));
        }
        let Some(v) = vm.iterator_step_value(iter, next)? else {
            return set_length(vm, arr, k);
        };
        let r: JsResult<()> = (|| {
            let v = if mapping {
                vm.call_value(mapfn, this_arg, &[v, Value::Number(k)])?
            } else {
                v
            };
            let key = vm.number_key(k);
            vm.create_data_property_or_throw(arr, key, v)
        })();
        if let Err(f) = r {
            vm.iterator_close_swallow(iter);
            return Err(f);
        }
        k += 1.0;
    }
}

// ---- prototype -----------------------------------------------------------------------

fn proto_at(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    let rel = vm.to_integer_or_infinity(a.arg(0))?;
    let k = if rel >= 0.0 { rel } else { len + rel };
    if k < 0.0 || k >= len {
        return Ok(Value::Undefined);
    }
    let key = vm.number_key(k);
    vm.get(o, key)
}

fn proto_concat(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = vm.to_object(a.this)?;
    let arr = array_species_create(vm, o, 0.0)?;
    let mut n = 0.0;
    let mut items = Vec::with_capacity(a.len() + 1);
    items.push(Value::Object(o));
    items.extend_from_slice(a.args);
    for e in items {
        if is_concat_spreadable(vm, e)? {
            let Value::Object(eo) = e else { unreachable!() };
            let len = vm.length_of_array_like(eo)?;
            if n + len > MAX_SAFE {
                return vm.type_error("Array.prototype.concat: result too large");
            }
            let mut k = 0.0;
            while k < len {
                let key = vm.number_key(k);
                if vm.has_property(eo, key)? {
                    let v = vm.get(eo, key)?;
                    let nk = vm.number_key(n);
                    vm.create_data_property_or_throw(arr, nk, v)?;
                }
                n += 1.0;
                k += 1.0;
            }
        } else {
            if n >= MAX_SAFE {
                return vm.type_error("Array.prototype.concat: result too large");
            }
            let nk = vm.number_key(n);
            vm.create_data_property_or_throw(arr, nk, e)?;
            n += 1.0;
        }
    }
    set_length(vm, arr, n)?;
    Ok(Value::Object(arr))
}

fn proto_copy_within(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    let to = relative_index(vm, a.arg(0), len, 0.0)?;
    let from = relative_index(vm, a.arg(1), len, 0.0)?;
    let fin = relative_index(vm, a.arg(2), len, len)?;
    let mut count = (fin - from).min(len - to);
    let (mut from, mut to, dir) = if from < to && to < from + count {
        (from + count - 1.0, to + count - 1.0, -1.0)
    } else {
        (from, to, 1.0)
    };
    while count > 0.0 {
        let fk = vm.number_key(from);
        let tk = vm.number_key(to);
        if vm.has_property(o, fk)? {
            let v = vm.get(o, fk)?;
            vm.set(o, tk, v, true)?;
        } else {
            vm.delete_property_or_throw(o, tk)?;
        }
        from += dir;
        to += dir;
        count -= 1.0;
    }
    Ok(Value::Object(o))
}

fn proto_entries(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = vm.to_object(a.this)?;
    Ok(create_array_iterator(vm, o, IterKind::Entries))
}

fn proto_keys(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = vm.to_object(a.this)?;
    Ok(create_array_iterator(vm, o, IterKind::Keys))
}

fn proto_values(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = vm.to_object(a.this)?;
    Ok(create_array_iterator(vm, o, IterKind::Values))
}

/// Keep a non-index key alive on the root stack for one iteration of a
/// callback series (`Vm::callback_series`): past 2^32 - 2 the key is a
/// string the native holds across the traps and getters it calls. Index
/// keys need nothing. An early return leaves it there; the series cuts
/// the stack back.
fn pin_key(vm: &mut Vm, key: crate::value::PropKey) {
    use crate::value::PropKey;
    match key {
        PropKey::Str(s) => vm.roots.push(Value::String(s)),
        PropKey::Sym(s) => vm.roots.push(Value::Symbol(s)),
        PropKey::Index(_) => {}
    }
}

fn unpin_key(vm: &mut Vm, key: crate::value::PropKey) {
    if !matches!(key, crate::value::PropKey::Index(_)) {
        vm.roots.pop();
    }
}

fn proto_every(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    let f = callback(vm, a.arg(0), "every")?;
    let this_arg = a.arg(1);
    vm.callback_series(&[Value::Object(o), f, this_arg], |vm| {
        let mut k = 0.0;
        while k < len {
            let key = vm.number_key(k);
            pin_key(vm, key);
            if vm.has_property(o, key)? {
                let v = vm.get(o, key)?;
                let r = vm.call_value(f, this_arg, &[v, Value::Number(k), Value::Object(o)])?;
                if !vm.to_boolean(r) {
                    return Ok(Value::Bool(false));
                }
            }
            unpin_key(vm, key);
            k += 1.0;
        }
        Ok(Value::Bool(true))
    })
}

fn proto_some(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    let f = callback(vm, a.arg(0), "some")?;
    let this_arg = a.arg(1);
    vm.callback_series(&[Value::Object(o), f, this_arg], |vm| {
        let mut k = 0.0;
        while k < len {
            let key = vm.number_key(k);
            pin_key(vm, key);
            if vm.has_property(o, key)? {
                let v = vm.get(o, key)?;
                let r = vm.call_value(f, this_arg, &[v, Value::Number(k), Value::Object(o)])?;
                if vm.to_boolean(r) {
                    return Ok(Value::Bool(true));
                }
            }
            unpin_key(vm, key);
            k += 1.0;
        }
        Ok(Value::Bool(false))
    })
}

fn proto_for_each(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    let f = callback(vm, a.arg(0), "forEach")?;
    let this_arg = a.arg(1);
    // A callback series (see `Vm::callback_series`): between calls this
    // holds only `o`, `f` and `this_arg`, which are rooted, and the key;
    // the element is held only as an argument of the call in progress.
    vm.callback_series(&[Value::Object(o), f, this_arg], |vm| {
        let mut k = 0.0;
        while k < len {
            let key = vm.number_key(k);
            pin_key(vm, key);
            if vm.has_property(o, key)? {
                let v = vm.get(o, key)?;
                vm.call_value(f, this_arg, &[v, Value::Number(k), Value::Object(o)])?;
            }
            unpin_key(vm, key);
            k += 1.0;
        }
        Ok(Value::Undefined)
    })
}

fn proto_fill(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    let start = relative_index(vm, a.arg(1), len, 0.0)?;
    let end = relative_index(vm, a.arg(2), len, len)?;
    let v = a.arg(0);
    let mut k = start;
    while k < end {
        let key = vm.number_key(k);
        vm.set(o, key, v, true)?;
        k += 1.0;
    }
    Ok(Value::Object(o))
}

fn proto_filter(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    let f = callback(vm, a.arg(0), "filter")?;
    let this_arg = a.arg(1);
    let arr = array_species_create(vm, o, 0.0)?;
    // Series: `arr` is created (species constructor and all) before it
    // begins and rooted; kept elements go into `arr` before the next call.
    vm.callback_series(&[Value::Object(o), f, this_arg, Value::Object(arr)], |vm| {
        let mut k = 0.0;
        let mut to = 0.0;
        while k < len {
            let key = vm.number_key(k);
            pin_key(vm, key);
            if vm.has_property(o, key)? {
                let v = vm.get(o, key)?;
                let r = vm.call_value(f, this_arg, &[v, Value::Number(k), Value::Object(o)])?;
                if vm.to_boolean(r) {
                    // `v` is held across the define (a trap may run):
                    // root it with the target key.
                    vm.roots.push(v);
                    let tk = vm.number_key(to);
                    pin_key(vm, tk);
                    vm.create_data_property_or_throw(arr, tk, v)?;
                    unpin_key(vm, tk);
                    vm.roots.pop();
                    to += 1.0;
                }
            }
            unpin_key(vm, key);
            k += 1.0;
        }
        Ok(Value::Object(arr))
    })
}

fn find_via_predicate(
    vm: &mut Vm,
    a: &CallArgs,
    from_end: bool,
    name: &str,
) -> JsResult<(f64, Value)> {
    let (o, len) = this_obj(vm, a)?;
    let f = callback(vm, a.arg(0), name)?;
    let this_arg = a.arg(1);
    // Series: the element is returned right after the call that found it,
    // with no safe point in between.
    vm.callback_series(&[Value::Object(o), f, this_arg], |vm| {
        let mut i = 0.0;
        while i < len {
            let k = if from_end { len - 1.0 - i } else { i };
            let key = vm.number_key(k);
            pin_key(vm, key);
            let v = vm.get(o, key)?;
            let r = vm.call_value(f, this_arg, &[v, Value::Number(k), Value::Object(o)])?;
            if vm.to_boolean(r) {
                return Ok((k, v));
            }
            unpin_key(vm, key);
            i += 1.0;
        }
        Ok((-1.0, Value::Undefined))
    })
}

fn proto_find(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    Ok(find_via_predicate(vm, a, false, "find")?.1)
}

fn proto_find_index(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    Ok(Value::Number(
        find_via_predicate(vm, a, false, "findIndex")?.0,
    ))
}

fn proto_find_last(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    Ok(find_via_predicate(vm, a, true, "findLast")?.1)
}

fn proto_find_last_index(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    Ok(Value::Number(
        find_via_predicate(vm, a, true, "findLastIndex")?.0,
    ))
}

/// FlattenIntoArray(target, source, sourceLen, start, depth, mapper, thisArg).
fn flatten_into_array(
    vm: &mut Vm,
    target: ObjId,
    source: ObjId,
    source_len: f64,
    start: f64,
    depth: f64,
    mapper: Option<(Value, Value)>,
) -> JsResult<f64> {
    let mut target_index = start;
    let mut si = 0.0;
    while si < source_len {
        let key = vm.number_key(si);
        if vm.has_property(source, key)? {
            let mut element = vm.get(source, key)?;
            if let Some((f, this_arg)) = mapper {
                element = vm.call_value(
                    f,
                    this_arg,
                    &[element, Value::Number(si), Value::Object(source)],
                )?;
            }
            let mut flatten = false;
            if depth > 0.0 {
                flatten = vm.is_array(element)?;
            }
            if flatten {
                let Value::Object(eo) = element else {
                    unreachable!()
                };
                let new_depth = if depth == f64::INFINITY {
                    depth
                } else {
                    depth - 1.0
                };
                let elen = vm.length_of_array_like(eo)?;
                target_index =
                    flatten_into_array(vm, target, eo, elen, target_index, new_depth, None)?;
            } else {
                if target_index >= MAX_SAFE {
                    return vm.type_error("Array.prototype.flat: result too large");
                }
                let tk = vm.number_key(target_index);
                vm.create_data_property_or_throw(target, tk, element)?;
                target_index += 1.0;
            }
        }
        si += 1.0;
    }
    Ok(target_index)
}

fn proto_flat(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    let depth = if a.arg(0).is_undefined() {
        1.0
    } else {
        let d = vm.to_integer_or_infinity(a.arg(0))?;
        if d < 0.0 {
            0.0
        } else {
            d
        }
    };
    let arr = array_species_create(vm, o, 0.0)?;
    flatten_into_array(vm, arr, o, len, 0.0, depth, None)?;
    Ok(Value::Object(arr))
}

fn proto_flat_map(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    let f = callback(vm, a.arg(0), "flatMap")?;
    let arr = array_species_create(vm, o, 0.0)?;
    flatten_into_array(vm, arr, o, len, 0.0, 1.0, Some((f, a.arg(1))))?;
    Ok(Value::Object(arr))
}

fn proto_includes(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    if len == 0.0 {
        return Ok(Value::Bool(false));
    }
    let n = vm.to_integer_or_infinity(a.arg(1))?;
    if n == f64::INFINITY {
        return Ok(Value::Bool(false));
    }
    let mut k = if n >= 0.0 { n } else { (len + n).max(0.0) };
    let target = a.arg(0);
    while k < len {
        let key = vm.number_key(k);
        let v = vm.get(o, key)?;
        if vm.same_value_zero(v, target) {
            return Ok(Value::Bool(true));
        }
        k += 1.0;
    }
    Ok(Value::Bool(false))
}

fn proto_index_of(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    if len == 0.0 {
        return Ok(Value::Number(-1.0));
    }
    let n = vm.to_integer_or_infinity(a.arg(1))?;
    if n == f64::INFINITY {
        return Ok(Value::Number(-1.0));
    }
    let mut k = if n >= 0.0 { n } else { (len + n).max(0.0) };
    let target = a.arg(0);
    while k < len {
        let key = vm.number_key(k);
        if vm.has_property(o, key)? {
            let v = vm.get(o, key)?;
            if vm.strict_equals(v, target) {
                return Ok(Value::Number(k));
            }
        }
        k += 1.0;
    }
    Ok(Value::Number(-1.0))
}

fn proto_last_index_of(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    if len == 0.0 {
        return Ok(Value::Number(-1.0));
    }
    let n = if a.len() > 1 {
        vm.to_integer_or_infinity(a.arg(1))?
    } else {
        len - 1.0
    };
    if n == f64::NEG_INFINITY {
        return Ok(Value::Number(-1.0));
    }
    let mut k = if n >= 0.0 { n.min(len - 1.0) } else { len + n };
    let target = a.arg(0);
    while k >= 0.0 {
        let key = vm.number_key(k);
        if vm.has_property(o, key)? {
            let v = vm.get(o, key)?;
            if vm.strict_equals(v, target) {
                return Ok(Value::Number(k));
            }
        }
        k -= 1.0;
    }
    Ok(Value::Number(-1.0))
}

fn proto_join(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    let sep: Vec<u16> = if a.arg(0).is_undefined() {
        vec![b',' as u16]
    } else {
        let s = vm.to_string(a.arg(0))?;
        vm.heap.units(s).to_vec()
    };
    let mut b = StrBuilder::new();
    let mut k = 0.0;
    while k < len {
        if k > 0.0 {
            b.push_slice(&sep);
        }
        let key = vm.number_key(k);
        let v = vm.get(o, key)?;
        if !v.is_nullish() {
            let s = vm.to_string(v)?;
            let u = vm.heap.units(s);
            b.push_units(&u);
        }
        k += 1.0;
    }
    Ok(vm.string_from_builder(b))
}

fn proto_map(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    let f = callback(vm, a.arg(0), "map")?;
    let this_arg = a.arg(1);
    let arr = array_species_create(vm, o, len)?;
    // Series: `arr` predates it and is rooted; each result is rooted
    // across its define (a trap may run) and then lives in `arr`.
    vm.callback_series(&[Value::Object(o), f, this_arg, Value::Object(arr)], |vm| {
        let mut k = 0.0;
        while k < len {
            let key = vm.number_key(k);
            pin_key(vm, key);
            if vm.has_property(o, key)? {
                let v = vm.get(o, key)?;
                let r = vm.call_value(f, this_arg, &[v, Value::Number(k), Value::Object(o)])?;
                vm.roots.push(r);
                vm.create_data_property_or_throw(arr, key, r)?;
                vm.roots.pop();
            }
            unpin_key(vm, key);
            k += 1.0;
        }
        Ok(Value::Object(arr))
    })
}

fn proto_pop(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    if len == 0.0 {
        set_length(vm, o, 0.0)?;
        return Ok(Value::Undefined);
    }
    let new_len = len - 1.0;
    let key = vm.number_key(new_len);
    let v = vm.get(o, key)?;
    vm.delete_property_or_throw(o, key)?;
    set_length(vm, o, new_len)?;
    Ok(v)
}

fn proto_push(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    let n = a.len() as f64;
    if len + n > MAX_SAFE {
        return vm.type_error("Array.prototype.push: length exceeds the maximum safe integer");
    }
    let mut l = len;
    for &v in a.args {
        let key = vm.number_key(l);
        vm.set(o, key, v, true)?;
        l += 1.0;
    }
    set_length(vm, o, l)?;
    Ok(Value::Number(l))
}

fn reduce_impl(vm: &mut Vm, a: &CallArgs, right: bool) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    let f = callback(vm, a.arg(0), if right { "reduceRight" } else { "reduce" })?;
    if len == 0.0 && a.len() < 2 {
        return vm.type_error("Reduce of empty array with no initial value");
    }
    let mut i = 0.0;
    let mut acc = Value::Undefined;
    if a.len() >= 2 {
        acc = a.arg(1);
    } else {
        let mut have_acc = false;
        while i < len {
            let k = if right { len - 1.0 - i } else { i };
            let key = vm.number_key(k);
            i += 1.0;
            if vm.has_property(o, key)? {
                acc = vm.get(o, key)?;
                have_acc = true;
                break;
            }
        }
        if !have_acc {
            return vm.type_error("Reduce of empty array with no initial value");
        }
    }
    while i < len {
        let k = if right { len - 1.0 - i } else { i };
        let key = vm.number_key(k);
        if vm.has_property(o, key)? {
            let v = vm.get(o, key)?;
            acc = vm.call_value(
                f,
                Value::Undefined,
                &[acc, v, Value::Number(k), Value::Object(o)],
            )?;
        }
        i += 1.0;
    }
    Ok(acc)
}

fn proto_reduce(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    reduce_impl(vm, a, false)
}

fn proto_reduce_right(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    reduce_impl(vm, a, true)
}

fn proto_reverse(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    let middle = (len / 2.0).floor();
    let mut lower = 0.0;
    while lower != middle {
        let upper = len - lower - 1.0;
        let lk = vm.number_key(lower);
        let uk = vm.number_key(upper);
        let lower_exists = vm.has_property(o, lk)?;
        let lower_v = if lower_exists {
            vm.get(o, lk)?
        } else {
            Value::Undefined
        };
        let upper_exists = vm.has_property(o, uk)?;
        let upper_v = if upper_exists {
            vm.get(o, uk)?
        } else {
            Value::Undefined
        };
        match (lower_exists, upper_exists) {
            (true, true) => {
                vm.set(o, lk, upper_v, true)?;
                vm.set(o, uk, lower_v, true)?;
            }
            (false, true) => {
                vm.set(o, lk, upper_v, true)?;
                vm.delete_property_or_throw(o, uk)?;
            }
            (true, false) => {
                vm.delete_property_or_throw(o, lk)?;
                vm.set(o, uk, lower_v, true)?;
            }
            (false, false) => {}
        }
        lower += 1.0;
    }
    Ok(Value::Object(o))
}

fn proto_shift(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    if len == 0.0 {
        set_length(vm, o, 0.0)?;
        return Ok(Value::Undefined);
    }
    let first = vm.get(o, PropKey::Index(0))?;
    let mut k = 1.0;
    while k < len {
        let from = vm.number_key(k);
        let to = vm.number_key(k - 1.0);
        if vm.has_property(o, from)? {
            let v = vm.get(o, from)?;
            vm.set(o, to, v, true)?;
        } else {
            vm.delete_property_or_throw(o, to)?;
        }
        k += 1.0;
    }
    let last = vm.number_key(len - 1.0);
    vm.delete_property_or_throw(o, last)?;
    set_length(vm, o, len - 1.0)?;
    Ok(first)
}

fn proto_unshift(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    let n = a.len() as f64;
    if n > 0.0 {
        if len + n > MAX_SAFE {
            return vm
                .type_error("Array.prototype.unshift: length exceeds the maximum safe integer");
        }
        let mut k = len;
        while k > 0.0 {
            let from = vm.number_key(k - 1.0);
            let to = vm.number_key(k + n - 1.0);
            if vm.has_property(o, from)? {
                let v = vm.get(o, from)?;
                vm.set(o, to, v, true)?;
            } else {
                vm.delete_property_or_throw(o, to)?;
            }
            k -= 1.0;
        }
        for (j, &v) in a.args.iter().enumerate() {
            let key = vm.number_key(j as f64);
            vm.set(o, key, v, true)?;
        }
    }
    set_length(vm, o, len + n)?;
    Ok(Value::Number(len + n))
}

fn proto_slice(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    let k = relative_index(vm, a.arg(0), len, 0.0)?;
    let fin = relative_index(vm, a.arg(1), len, len)?;
    let count = (fin - k).max(0.0);
    let arr = array_species_create(vm, o, count)?;
    let mut k = k;
    let mut n = 0.0;
    while k < fin {
        let key = vm.number_key(k);
        if vm.has_property(o, key)? {
            let v = vm.get(o, key)?;
            let nk = vm.number_key(n);
            vm.create_data_property_or_throw(arr, nk, v)?;
        }
        k += 1.0;
        n += 1.0;
    }
    set_length(vm, arr, n)?;
    Ok(Value::Object(arr))
}

/// SortCompare with an optional comparator.
fn sort_compare(vm: &mut Vm, comparefn: Value, x: Value, y: Value) -> JsResult<Ordering> {
    match (x.is_undefined(), y.is_undefined()) {
        (true, true) => return Ok(Ordering::Equal),
        (true, false) => return Ok(Ordering::Greater),
        (false, true) => return Ok(Ordering::Less),
        _ => {}
    }
    if !comparefn.is_undefined() {
        let r = vm.call_value(comparefn, Value::Undefined, &[x, y])?;
        let n = vm.to_number(r)?;
        return Ok(if n < 0.0 {
            Ordering::Less
        } else if n > 0.0 {
            Ordering::Greater
        } else {
            Ordering::Equal
        });
    }
    let xs = vm.to_string(x)?;
    let ys = vm.to_string(y)?;
    vm.heap.flatten(xs);
    vm.heap.flatten(ys);
    let xu = vm.heap.units_flat(xs);
    let yu = vm.heap.units_flat(ys);
    Ok(xu.cmp(&yu))
}

/// Stable merge sort with a fallible comparator.
pub fn sort_values(
    vm: &mut Vm,
    items: Vec<Value>,
    mut cmp: impl FnMut(&mut Vm, Value, Value) -> JsResult<Ordering>,
) -> JsResult<Vec<Value>> {
    let n = items.len();
    if n < 2 {
        return Ok(items);
    }
    let mut src = items;
    let mut dst = vec![Value::Undefined; n];
    let mut width = 1;
    while width < n {
        let mut i = 0;
        while i < n {
            let mid = (i + width).min(n);
            let end = (i + 2 * width).min(n);
            let (mut l, mut r, mut k) = (i, mid, i);
            while l < mid && r < end {
                if cmp(vm, src[r], src[l])? == Ordering::Less {
                    dst[k] = src[r];
                    r += 1;
                } else {
                    dst[k] = src[l];
                    l += 1;
                }
                k += 1;
            }
            while l < mid {
                dst[k] = src[l];
                l += 1;
                k += 1;
            }
            while r < end {
                dst[k] = src[r];
                r += 1;
                k += 1;
            }
            i += 2 * width;
        }
        std::mem::swap(&mut src, &mut dst);
        width *= 2;
    }
    Ok(src)
}

/// SortIndexedProperties(obj, len, SortCompare, holes).
fn sort_indexed_properties(
    vm: &mut Vm,
    o: ObjId,
    len: f64,
    comparefn: Value,
    skip_holes: bool,
) -> JsResult<Vec<Value>> {
    let mut items = Vec::new();
    let mut k = 0.0;
    while k < len {
        let key = vm.number_key(k);
        if skip_holes {
            if vm.has_property(o, key)? {
                items.push(vm.get(o, key)?);
            }
        } else {
            items.push(vm.get(o, key)?);
        }
        k += 1.0;
    }
    let mark = vm.root_mark();
    for &v in &items {
        vm.root(v);
    }
    let r = sort_values(vm, items, |vm, x, y| sort_compare(vm, comparefn, x, y));
    vm.pop_roots(mark);
    r
}

fn proto_sort(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let comparefn = a.arg(0);
    if !comparefn.is_undefined() && !vm.is_callable(comparefn) {
        return vm.type_error("The comparison function must be either a function or undefined");
    }
    let (o, len) = this_obj(vm, a)?;
    let sorted = sort_indexed_properties(vm, o, len, comparefn, true)?;
    let item_count = sorted.len() as f64;
    for (j, v) in sorted.into_iter().enumerate() {
        let key = vm.number_key(j as f64);
        vm.set(o, key, v, true)?;
    }
    let mut j = item_count;
    while j < len {
        let key = vm.number_key(j);
        vm.delete_property_or_throw(o, key)?;
        j += 1.0;
    }
    Ok(Value::Object(o))
}

fn proto_splice(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    let start = relative_index(vm, a.arg(0), len, 0.0)?;
    let items: &[Value] = if a.len() > 2 { &a.args[2..] } else { &[] };
    let insert_count = items.len() as f64;
    let delete_count = if a.is_empty() {
        0.0
    } else if a.len() == 1 {
        len - start
    } else {
        let dc = vm.to_integer_or_infinity(a.arg(1))?;
        dc.max(0.0).min(len - start)
    };
    if len + insert_count - delete_count > MAX_SAFE {
        return vm.type_error("Array.prototype.splice: length exceeds the maximum safe integer");
    }
    let arr = array_species_create(vm, o, delete_count)?;
    let mut k = 0.0;
    while k < delete_count {
        let from = vm.number_key(start + k);
        if vm.has_property(o, from)? {
            let v = vm.get(o, from)?;
            let to = vm.number_key(k);
            vm.create_data_property_or_throw(arr, to, v)?;
        }
        k += 1.0;
    }
    set_length(vm, arr, delete_count)?;
    if insert_count < delete_count {
        let mut k = start;
        while k < len - delete_count {
            let from = vm.number_key(k + delete_count);
            let to = vm.number_key(k + insert_count);
            if vm.has_property(o, from)? {
                let v = vm.get(o, from)?;
                vm.set(o, to, v, true)?;
            } else {
                vm.delete_property_or_throw(o, to)?;
            }
            k += 1.0;
        }
        let mut k = len;
        while k > len - delete_count + insert_count {
            let key = vm.number_key(k - 1.0);
            vm.delete_property_or_throw(o, key)?;
            k -= 1.0;
        }
    } else if insert_count > delete_count {
        let mut k = len - delete_count;
        while k > start {
            let from = vm.number_key(k + delete_count - 1.0);
            let to = vm.number_key(k + insert_count - 1.0);
            if vm.has_property(o, from)? {
                let v = vm.get(o, from)?;
                vm.set(o, to, v, true)?;
            } else {
                vm.delete_property_or_throw(o, to)?;
            }
            k -= 1.0;
        }
    }
    for (i, &v) in items.iter().enumerate() {
        let key = vm.number_key(start + i as f64);
        vm.set(o, key, v, true)?;
    }
    set_length(vm, o, len - delete_count + insert_count)?;
    Ok(Value::Object(arr))
}

fn proto_to_locale_string(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    let mut b = StrBuilder::new();
    let mut k = 0.0;
    while k < len {
        if k > 0.0 {
            b.push(b',' as u16);
        }
        let key = vm.number_key(k);
        let v = vm.get(o, key)?;
        if !v.is_nullish() {
            let r = vm.invoke_str(v, "toLocaleString", &[a.arg(0), a.arg(1)])?;
            let s = vm.to_string(r)?;
            let u = vm.heap.units(s);
            b.push_units(&u);
        }
        k += 1.0;
    }
    Ok(vm.string_from_builder(b))
}

fn proto_to_string(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = vm.to_object(a.this)?;
    let join_key = PropKey::Str(vm.names.join);
    let func = vm.get(o, join_key)?;
    if vm.is_callable(func) {
        return vm.call_value(func, Value::Object(o), &[]);
    }
    let args = CallArgs {
        this: Value::Object(o),
        args: &[],
        new_target: Value::Undefined,
        callee: a.callee,
    };
    super::object::proto_to_string(vm, &args)
}

fn proto_to_reversed(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    let arr = vm.array_create(len, None)?;
    let mut k = 0.0;
    while k < len {
        let from = vm.number_key(len - k - 1.0);
        let v = vm.get(o, from)?;
        let to = vm.number_key(k);
        vm.create_data_property_or_throw(arr, to, v)?;
        k += 1.0;
    }
    Ok(Value::Object(arr))
}

fn proto_to_sorted(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let comparefn = a.arg(0);
    if !comparefn.is_undefined() && !vm.is_callable(comparefn) {
        return vm.type_error("The comparison function must be either a function or undefined");
    }
    let (o, len) = this_obj(vm, a)?;
    let arr = vm.array_create(len, None)?;
    let mark = vm.root_mark();
    vm.root(Value::Object(arr));
    let sorted = sort_indexed_properties(vm, o, len, comparefn, false);
    vm.pop_roots(mark);
    for (j, v) in sorted?.into_iter().enumerate() {
        let key = vm.number_key(j as f64);
        vm.create_data_property_or_throw(arr, key, v)?;
    }
    Ok(Value::Object(arr))
}

fn proto_to_spliced(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    let start = relative_index(vm, a.arg(0), len, 0.0)?;
    let items: &[Value] = if a.len() > 2 { &a.args[2..] } else { &[] };
    let insert_count = items.len() as f64;
    let skip_count = if a.is_empty() {
        0.0
    } else if a.len() == 1 {
        len - start
    } else {
        let dc = vm.to_integer_or_infinity(a.arg(1))?;
        dc.max(0.0).min(len - start)
    };
    let new_len = len + insert_count - skip_count;
    if new_len > MAX_SAFE {
        return vm.type_error("Array.prototype.toSpliced: length exceeds the maximum safe integer");
    }
    let arr = vm.array_create(new_len, None)?;
    let mut i = 0.0;
    let mut r = start + skip_count;
    while i < start {
        let key = vm.number_key(i);
        let v = vm.get(o, key)?;
        vm.create_data_property_or_throw(arr, key, v)?;
        i += 1.0;
    }
    for &v in items {
        let key = vm.number_key(i);
        vm.create_data_property_or_throw(arr, key, v)?;
        i += 1.0;
    }
    while i < new_len {
        let from = vm.number_key(r);
        let v = vm.get(o, from)?;
        let to = vm.number_key(i);
        vm.create_data_property_or_throw(arr, to, v)?;
        i += 1.0;
        r += 1.0;
    }
    Ok(Value::Object(arr))
}

fn proto_with(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = this_obj(vm, a)?;
    let rel = vm.to_integer_or_infinity(a.arg(0))?;
    let actual = if rel >= 0.0 { rel } else { len + rel };
    if actual >= len || actual < 0.0 {
        return vm.range_error("Array.prototype.with: index out of range");
    }
    let arr = vm.array_create(len, None)?;
    let mut k = 0.0;
    while k < len {
        let key = vm.number_key(k);
        let v = if k == actual {
            a.arg(1)
        } else {
            vm.get(o, key)?
        };
        vm.create_data_property_or_throw(arr, key, v)?;
        k += 1.0;
    }
    Ok(Value::Object(arr))
}

#[allow(dead_code)]
fn _int(v: f64) -> f64 {
    to_integer_or_infinity_f64(v)
}
