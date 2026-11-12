//! %TypedArray%, its prototype and the twelve concrete constructors
//! (ES2025 §23.2). The element model and the integer-indexed exotic
//! hooks live in `typedarray.rs`; this file is the constructor protocol
//! and the prototype methods.

use std::cmp::Ordering;

use agnijs_engine_api::RealmId;

use super::arraybuffer::{
    buffer_byte_length, buffer_data, is_detached, kind_from_tag, new_array_buffer,
};
use super::typedarray::{
    ta_get_element, ta_length, ta_set_element, ta_store, valid_index, Converted, ElementKind,
    TypedArrayData,
};
use crate::builtins::iterator::create_array_iterator;
use crate::object::{CallArgs, Class, IterKind, ObjectData};
use crate::realm::Intrinsic;
use crate::string::StrBuilder;
use crate::value::{Attrs, ObjId, PropKey, Value};
use crate::vm::{JsResult, Vm};

pub const KINDS: [ElementKind; 12] = [
    ElementKind::Int8,
    ElementKind::Uint8,
    ElementKind::Uint8Clamped,
    ElementKind::Int16,
    ElementKind::Uint16,
    ElementKind::Int32,
    ElementKind::Uint32,
    ElementKind::Float16,
    ElementKind::Float32,
    ElementKind::Float64,
    ElementKind::BigInt64,
    ElementKind::BigUint64,
];

pub fn kind_name(kind: ElementKind) -> &'static str {
    match kind {
        ElementKind::Int8 => "Int8Array",
        ElementKind::Uint8 => "Uint8Array",
        ElementKind::Uint8Clamped => "Uint8ClampedArray",
        ElementKind::Int16 => "Int16Array",
        ElementKind::Uint16 => "Uint16Array",
        ElementKind::Int32 => "Int32Array",
        ElementKind::Uint32 => "Uint32Array",
        ElementKind::Float16 => "Float16Array",
        ElementKind::Float32 => "Float32Array",
        ElementKind::Float64 => "Float64Array",
        ElementKind::BigInt64 => "BigInt64Array",
        ElementKind::BigUint64 => "BigUint64Array",
    }
}

pub fn kind_tag(kind: ElementKind) -> u8 {
    KINDS.iter().position(|k| *k == kind).unwrap() as u8
}

pub fn kind_intrinsics(kind: ElementKind) -> (Intrinsic, Intrinsic) {
    match kind {
        ElementKind::Int8 => (Intrinsic::Int8Array, Intrinsic::Int8ArrayPrototype),
        ElementKind::Uint8 => (Intrinsic::Uint8Array, Intrinsic::Uint8ArrayPrototype),
        ElementKind::Uint8Clamped => (
            Intrinsic::Uint8ClampedArray,
            Intrinsic::Uint8ClampedArrayPrototype,
        ),
        ElementKind::Int16 => (Intrinsic::Int16Array, Intrinsic::Int16ArrayPrototype),
        ElementKind::Uint16 => (Intrinsic::Uint16Array, Intrinsic::Uint16ArrayPrototype),
        ElementKind::Int32 => (Intrinsic::Int32Array, Intrinsic::Int32ArrayPrototype),
        ElementKind::Uint32 => (Intrinsic::Uint32Array, Intrinsic::Uint32ArrayPrototype),
        ElementKind::Float16 => (Intrinsic::Float16Array, Intrinsic::Float16ArrayPrototype),
        ElementKind::Float32 => (Intrinsic::Float32Array, Intrinsic::Float32ArrayPrototype),
        ElementKind::Float64 => (Intrinsic::Float64Array, Intrinsic::Float64ArrayPrototype),
        ElementKind::BigInt64 => (Intrinsic::BigInt64Array, Intrinsic::BigInt64ArrayPrototype),
        ElementKind::BigUint64 => (
            Intrinsic::BigUint64Array,
            Intrinsic::BigUint64ArrayPrototype,
        ),
    }
}

// ---- shared helpers ----------------------------------------------------------------------

pub fn ta_data(vm: &Vm, o: ObjId) -> Option<&TypedArrayData> {
    match &vm.heap.obj(o).class {
        Class::TypedArray(t) => Some(t),
        _ => None,
    }
}

pub fn ta_kind(vm: &Vm, o: ObjId) -> ElementKind {
    ta_data(vm, o).map(|t| t.kind).unwrap_or(ElementKind::Uint8)
}

fn require_ta(vm: &mut Vm, v: Value, method: &str) -> JsResult<ObjId> {
    if let Value::Object(o) = v {
        if ta_data(vm, o).is_some() {
            return Ok(o);
        }
    }
    vm.type_error(&format!(
        "%TypedArray%.prototype.{method} called on incompatible receiver"
    ))
}

/// ValidateTypedArray: the object and its current length.
pub fn validate_typed_array(vm: &mut Vm, v: Value, method: &str) -> JsResult<(ObjId, usize)> {
    let o = require_ta(vm, v, method)?;
    match ta_length(vm, o) {
        Some(n) => Ok((o, n)),
        None => vm.type_error(&format!(
            "%TypedArray%.prototype.{method}: typed array is detached or out of bounds"
        )),
    }
}

/// A typed array of `kind` over a fresh buffer of `len` elements.
pub fn allocate_typed_array(vm: &mut Vm, kind: ElementKind, proto: ObjId, len: f64) -> JsResult<ObjId> {
    let buffer = new_array_buffer(vm, len * kind.size() as f64)?;
    let data = TypedArrayData {
        buffer,
        kind,
        byte_offset: 0,
        length: Some(len as usize),
    };
    Ok(vm.alloc(ObjectData::new(
        Some(proto),
        Class::TypedArray(Box::new(data)),
    )))
}

/// TypedArrayCreateFromConstructor(C, args).
pub fn typed_array_create_from_ctor(vm: &mut Vm, ctor: Value, args: &[Value]) -> JsResult<ObjId> {
    let new = vm.construct_value(ctor, args, ctor)?;
    let (o, len) = validate_typed_array(vm, new, "constructor")?;
    if let [Value::Number(n)] = args {
        if (len as f64) < *n {
            return vm.type_error("Derived TypedArray constructor created an array which was too small");
        }
    }
    Ok(o)
}

/// TypedArrayCreateSameType(exemplar, len).
pub fn typed_array_create_same_type(vm: &mut Vm, exemplar: ObjId, len: f64) -> JsResult<ObjId> {
    let kind = ta_kind(vm, exemplar);
    let ctor = vm.intrinsic(kind_intrinsics(kind).0);
    typed_array_create_from_ctor(vm, Value::Object(ctor), &[Value::Number(len)])
}

/// TypedArraySpeciesCreate(exemplar, args).
pub fn typed_array_species_create(vm: &mut Vm, exemplar: ObjId, args: &[Value]) -> JsResult<ObjId> {
    let kind = ta_kind(vm, exemplar);
    let default = kind_intrinsics(kind).0;
    let ctor = vm.species_constructor(exemplar, default)?;
    let result = typed_array_create_from_ctor(vm, ctor, args)?;
    if ta_kind(vm, result).is_bigint() != kind.is_bigint() {
        return vm.type_error("TypedArray species constructor returned an array with a different content type");
    }
    Ok(result)
}

/// Get(O, k) for an element index: `undefined` once the index is no longer valid.
fn elem(vm: &mut Vm, o: ObjId, i: usize) -> Value {
    match valid_index(vm, o, i as f64) {
        Some(i) => ta_get_element(vm, o, i),
        None => Value::Undefined,
    }
}

fn set_elem(vm: &mut Vm, o: ObjId, i: usize, v: Value) -> JsResult<()> {
    vm.set(o, PropKey::Index(i as u32), v, true)
}

fn callable_arg(vm: &mut Vm, v: Value, method: &str) -> JsResult<Value> {
    if !vm.is_callable(v) {
        return vm.type_error(&format!("%TypedArray%.prototype.{method}: callback is not a function"));
    }
    Ok(v)
}

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

/// Converted form of a value already of the array's content type.
fn converted(vm: &Vm, v: Value) -> Converted {
    match v {
        Value::Number(n) => Converted::Num(n),
        Value::BigInt(b) => Converted::Big(crate::bigint::to_i128_wrapping(&vm.heap.bigint(b).0)),
        _ => Converted::Num(f64::NAN),
    }
}

fn iterator_from_method(vm: &mut Vm, v: Value, m: Value) -> JsResult<(Value, Value)> {
    let it = vm.call_value(m, v, &[])?;
    if !it.is_object() {
        return vm.type_error("Result of the Symbol.iterator method is not an object");
    }
    let next = vm.get_value(it, PropKey::Str(vm.names.next))?;
    Ok((it, next))
}

fn list_from_iterator(vm: &mut Vm, it: Value, next: Value) -> JsResult<Vec<Value>> {
    let mut out = Vec::new();
    while let Some(x) = vm.iterator_step_value(it, next)? {
        out.push(x);
    }
    Ok(out)
}

// ---- install -----------------------------------------------------------------------------

pub fn install(vm: &mut Vm, realm: RealmId) {
    let proto = vm.new_object();
    let ctor = super::install_ctor(
        vm,
        realm,
        "TypedArray",
        0,
        typed_array_abstract_constructor,
        proto,
        Intrinsic::TypedArray,
        Intrinsic::TypedArrayPrototype,
    );
    // %TypedArray% is not a global.
    let global = vm.realm_of(realm).global;
    let key = vm.key("TypedArray");
    vm.heap.remove_own_property(global, key);
    vm.method(ctor, "from", 1, ta_from);
    vm.method(ctor, "of", 0, ta_of);
    super::species_accessor(vm, ctor);

    vm.accessor(proto, "buffer", get_buffer, None);
    vm.accessor(proto, "byteLength", get_byte_length, None);
    vm.accessor(proto, "byteOffset", get_byte_offset, None);
    vm.accessor(proto, "length", get_length, None);
    let tag = vm.well_known.to_string_tag;
    vm.accessor_sym(proto, tag, "Symbol.toStringTag", get_to_string_tag);

    vm.method(proto, "at", 1, proto_at);
    vm.method(proto, "copyWithin", 2, proto_copy_within);
    vm.method(proto, "entries", 0, proto_entries);
    vm.method(proto, "every", 1, proto_every);
    vm.method(proto, "fill", 1, proto_fill);
    vm.method(proto, "filter", 1, proto_filter);
    vm.method(proto, "find", 1, proto_find);
    vm.method(proto, "findIndex", 1, proto_find_index);
    vm.method(proto, "findLast", 1, proto_find_last);
    vm.method(proto, "findLastIndex", 1, proto_find_last_index);
    vm.method(proto, "forEach", 1, proto_for_each);
    vm.method(proto, "includes", 1, proto_includes);
    vm.method(proto, "indexOf", 1, proto_index_of);
    vm.method(proto, "join", 1, proto_join);
    vm.method(proto, "keys", 0, proto_keys);
    vm.method(proto, "lastIndexOf", 1, proto_last_index_of);
    vm.method(proto, "map", 1, proto_map);
    vm.method(proto, "reduce", 1, proto_reduce);
    vm.method(proto, "reduceRight", 1, proto_reduce_right);
    vm.method(proto, "reverse", 0, proto_reverse);
    vm.method(proto, "set", 1, proto_set);
    vm.method(proto, "slice", 2, proto_slice);
    vm.method(proto, "some", 1, proto_some);
    vm.method(proto, "sort", 1, proto_sort);
    vm.method(proto, "subarray", 2, proto_subarray);
    vm.method(proto, "toLocaleString", 0, proto_to_locale_string);
    vm.method(proto, "toReversed", 0, proto_to_reversed);
    vm.method(proto, "toSorted", 1, proto_to_sorted);
    let values = vm.method(proto, "values", 0, proto_values);
    vm.method(proto, "with", 2, proto_with);
    let iter = vm.well_known.iterator;
    vm.define_raw(proto, PropKey::Sym(iter), Value::Object(values), Attrs::METHOD);
    // toString is the same function object as Array.prototype.toString.
    let array_proto = vm.intrinsic(Intrinsic::ArrayPrototype);
    let ts_key = PropKey::Str(vm.names.toString);
    if let Some(p) = vm.heap.get_own_property(array_proto, ts_key) {
        if let Some(v) = p.value() {
            vm.define_raw(proto, ts_key, v, Attrs::METHOD);
        }
    }

    for kind in KINDS {
        let (ci, pi) = kind_intrinsics(kind);
        let name = kind_name(kind);
        let kproto = vm.new_object_with_proto(Some(proto));
        let kctor = super::install_ctor(vm, realm, name, 3, typed_array_constructor, kproto, ci, pi);
        vm.heap.set_proto(kctor, Some(ctor));
        vm.set_native_slot(kctor, 0, Value::Number(kind_tag(kind) as f64));
        let bpe = Value::Number(kind.size() as f64);
        vm.define_raw_str(kctor, "BYTES_PER_ELEMENT", bpe, Attrs::NONE);
        vm.define_raw_str(kproto, "BYTES_PER_ELEMENT", bpe, Attrs::NONE);
    }
}

// ---- constructors ------------------------------------------------------------------------

fn typed_array_abstract_constructor(vm: &mut Vm, _a: &CallArgs) -> JsResult<Value> {
    vm.type_error("Abstract class TypedArray not directly constructable")
}

fn typed_array_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let kind = kind_from_tag(match vm.native_slot(a.callee, 0) {
        Value::Number(n) => n as u8,
        _ => 0,
    });
    let name = kind_name(kind);
    if a.new_target.is_undefined() {
        return vm.type_error(&format!("Constructor {name} requires 'new'"));
    }
    let (_, proto_intrinsic) = kind_intrinsics(kind);
    let first = a.arg(0);
    let Value::Object(src) = first else {
        let len = vm.to_index(first)?;
        let proto = vm.get_prototype_from_constructor(a.new_target, proto_intrinsic)?;
        return Ok(Value::Object(allocate_typed_array(vm, kind, proto, len)?));
    };
    let proto = vm.get_prototype_from_constructor(a.new_target, proto_intrinsic)?;
    let size = kind.size();
    // From another typed array.
    if let Some(sd) = ta_data(vm, src) {
        let src_kind = sd.kind;
        let Some(src_len) = ta_length(vm, src) else {
            return vm.type_error("Source typed array is detached or out of bounds");
        };
        if src_kind.is_bigint() != kind.is_bigint() {
            return vm.type_error("Cannot mix BigInt and other types");
        }
        let o = allocate_typed_array(vm, kind, proto, src_len as f64)?;
        let (src_buf, src_off) = {
            let sd = ta_data(vm, src).unwrap();
            (sd.buffer, sd.byte_offset)
        };
        if src_kind == kind {
            let bytes: Vec<u8> = buffer_data(vm, src_buf).unwrap().bytes
                [src_off..src_off + src_len * size]
                .to_vec();
            let dst_buf = ta_data(vm, o).unwrap().buffer;
            if let Class::ArrayBuffer(b) = &mut vm.heap.obj_mut(dst_buf).class {
                b.bytes[..bytes.len()].copy_from_slice(&bytes);
            }
        } else {
            for i in 0..src_len {
                let v = ta_get_element(vm, src, i);
                let c = converted(vm, v);
                ta_store(vm, o, i, c);
            }
        }
        return Ok(Value::Object(o));
    }
    // From an ArrayBuffer.
    if buffer_data(vm, src).is_some() {
        let offset = vm.to_index(a.arg(1))?;
        if offset as usize % size != 0 {
            return vm.range_error(&format!(
                "start offset of {name} should be a multiple of {size}"
            ));
        }
        let length_arg = a.arg(2);
        let new_length = if length_arg.is_undefined() {
            None
        } else {
            Some(vm.to_index(length_arg)?)
        };
        if is_detached(vm, src) {
            return vm.type_error("ArrayBuffer is detached");
        }
        let buf_len = buffer_byte_length(vm, src) as f64;
        let resizable = buffer_data(vm, src).unwrap().max_byte_length.is_some();
        let length = match new_length {
            None if resizable => {
                if offset > buf_len {
                    return vm.range_error("Start offset is outside the bounds of the buffer");
                }
                None
            }
            None => {
                if buf_len as usize % size != 0 {
                    return vm.range_error(&format!(
                        "byte length of {name} should be a multiple of {size}"
                    ));
                }
                let nbl = buf_len - offset;
                if nbl < 0.0 {
                    return vm.range_error("Start offset is outside the bounds of the buffer");
                }
                Some((nbl / size as f64) as usize)
            }
            Some(l) => {
                if offset + l * size as f64 > buf_len {
                    return vm.range_error("Invalid typed array length");
                }
                Some(l as usize)
            }
        };
        let data = TypedArrayData {
            buffer: src,
            kind,
            byte_offset: offset as usize,
            length,
        };
        return Ok(Value::Object(vm.alloc(ObjectData::new(
            Some(proto),
            Class::TypedArray(Box::new(data)),
        ))));
    }
    // From an iterable or array-like.
    let iter_key = PropKey::Sym(vm.well_known.iterator);
    let using = vm.get_method(first, iter_key)?;
    let values: Vec<Value>;
    let len;
    if let Some(m) = using {
        let (it, next) = iterator_from_method(vm, first, m)?;
        values = list_from_iterator(vm, it, next)?;
        len = values.len();
    } else {
        len = vm.length_of_array_like(src)? as usize;
        values = Vec::new();
    }
    let o = allocate_typed_array(vm, kind, proto, len as f64)?;
    if using.is_some() {
        for (i, v) in values.into_iter().enumerate() {
            set_elem(vm, o, i, v)?;
        }
    } else {
        for i in 0..len {
            let v = vm.get(src, PropKey::Index(i as u32))?;
            set_elem(vm, o, i, v)?;
        }
    }
    Ok(Value::Object(o))
}

fn ta_from(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let c = a.this;
    if !vm.is_constructor(c) {
        return vm.type_error("%TypedArray%.from: this is not a constructor");
    }
    let mapfn = a.arg(1);
    let mapping = !mapfn.is_undefined();
    if mapping && !vm.is_callable(mapfn) {
        return vm.type_error("%TypedArray%.from: mapper is not a function");
    }
    let this_arg = a.arg(2);
    let source = a.arg(0);
    let iter_key = PropKey::Sym(vm.well_known.iterator);
    let using = vm.get_method(source, iter_key)?;
    if let Some(m) = using {
        let (it, next) = iterator_from_method(vm, source, m)?;
        let values = list_from_iterator(vm, it, next)?;
        let len = values.len();
        let target = typed_array_create_from_ctor(vm, c, &[Value::Number(len as f64)])?;
        for (k, v) in values.into_iter().enumerate() {
            let mapped = if mapping {
                vm.call_value(mapfn, this_arg, &[v, Value::Number(k as f64)])?
            } else {
                v
            };
            set_elem(vm, target, k, mapped)?;
        }
        return Ok(Value::Object(target));
    }
    let array_like = vm.to_object(source)?;
    let len = vm.length_of_array_like(array_like)?;
    let target = typed_array_create_from_ctor(vm, c, &[Value::Number(len)])?;
    let mut k = 0.0;
    while k < len {
        let key = vm.number_key(k);
        let v = vm.get(array_like, key)?;
        let mapped = if mapping {
            vm.call_value(mapfn, this_arg, &[v, Value::Number(k)])?
        } else {
            v
        };
        vm.set(target, key, mapped, true)?;
        k += 1.0;
    }
    Ok(Value::Object(target))
}

fn ta_of(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let c = a.this;
    if !vm.is_constructor(c) {
        return vm.type_error("%TypedArray%.of: this is not a constructor");
    }
    let len = a.args.len();
    let target = typed_array_create_from_ctor(vm, c, &[Value::Number(len as f64)])?;
    for (k, v) in a.args.iter().enumerate() {
        set_elem(vm, target, k, *v)?;
    }
    Ok(Value::Object(target))
}

// ---- accessors ---------------------------------------------------------------------------

fn get_buffer(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = require_ta(vm, a.this, "buffer")?;
    Ok(Value::Object(ta_data(vm, o).unwrap().buffer))
}

fn get_byte_length(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = require_ta(vm, a.this, "byteLength")?;
    let size = ta_kind(vm, o).size();
    Ok(Value::Number((ta_length(vm, o).unwrap_or(0) * size) as f64))
}

fn get_byte_offset(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = require_ta(vm, a.this, "byteOffset")?;
    if ta_length(vm, o).is_none() {
        return Ok(Value::Number(0.0));
    }
    Ok(Value::Number(ta_data(vm, o).unwrap().byte_offset as f64))
}

fn get_length(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = require_ta(vm, a.this, "length")?;
    Ok(Value::Number(ta_length(vm, o).unwrap_or(0) as f64))
}

fn get_to_string_tag(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Value::Object(o) = a.this else {
        return Ok(Value::Undefined);
    };
    match ta_data(vm, o) {
        Some(t) => {
            let name = kind_name(t.kind);
            Ok(vm.str(name))
        }
        None => Ok(Value::Undefined),
    }
}

// ---- prototype methods -------------------------------------------------------------------

fn proto_at(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = validate_typed_array(vm, a.this, "at")?;
    let rel = vm.to_integer_or_infinity(a.arg(0))?;
    let k = if rel >= 0.0 { rel } else { len as f64 + rel };
    if k < 0.0 || k >= len as f64 {
        return Ok(Value::Undefined);
    }
    Ok(elem(vm, o, k as usize))
}

fn proto_copy_within(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = validate_typed_array(vm, a.this, "copyWithin")?;
    let lenf = len as f64;
    let to = relative_index(vm, a.arg(0), lenf, 0.0)?;
    let from = relative_index(vm, a.arg(1), lenf, 0.0)?;
    let fin = relative_index(vm, a.arg(2), lenf, lenf)?;
    let count = (fin - from).min(lenf - to);
    if count > 0.0 {
        let Some(len) = ta_length(vm, o) else {
            return vm.type_error("%TypedArray%.prototype.copyWithin: typed array is detached or out of bounds");
        };
        let (buffer, kind, offset) = {
            let t = ta_data(vm, o).unwrap();
            (t.buffer, t.kind, t.byte_offset)
        };
        let size = kind.size();
        let limit = len * size + offset;
        let to_b = to as usize * size + offset;
        let from_b = from as usize * size + offset;
        let mut count_b = count as usize * size;
        if from_b + count_b > limit || to_b + count_b > limit {
            count_b = limit.saturating_sub(from_b).min(limit.saturating_sub(to_b));
        }
        if let Class::ArrayBuffer(b) = &mut vm.heap.obj_mut(buffer).class {
            b.bytes.copy_within(from_b..from_b + count_b, to_b);
        }
    }
    Ok(Value::Object(o))
}

fn proto_entries(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, _) = validate_typed_array(vm, a.this, "entries")?;
    Ok(create_array_iterator(vm, o, IterKind::Entries))
}

fn proto_keys(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, _) = validate_typed_array(vm, a.this, "keys")?;
    Ok(create_array_iterator(vm, o, IterKind::Keys))
}

fn proto_values(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, _) = validate_typed_array(vm, a.this, "values")?;
    Ok(create_array_iterator(vm, o, IterKind::Values))
}

/// Shared loop for every/some/forEach/find*: calls `f(value, index, O)`
/// and stops when `stop` returns true for the boolean result.
fn iterate(
    vm: &mut Vm,
    a: &CallArgs,
    method: &str,
    reverse: bool,
    mut on_result: impl FnMut(&mut Vm, Value, usize, bool) -> Option<Value>,
) -> JsResult<Option<Value>> {
    let (o, len) = validate_typed_array(vm, a.this, method)?;
    let cb = callable_arg(vm, a.arg(0), method)?;
    let this_arg = a.arg(1);
    for step in 0..len {
        let k = if reverse { len - 1 - step } else { step };
        let v = elem(vm, o, k);
        let r = vm.call_value(cb, this_arg, &[v, Value::Number(k as f64), Value::Object(o)])?;
        let b = vm.to_boolean(r);
        if let Some(out) = on_result(vm, v, k, b) {
            return Ok(Some(out));
        }
    }
    Ok(None)
}

fn proto_every(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let r = iterate(vm, a, "every", false, |_, _, _, b| (!b).then_some(Value::Bool(false)))?;
    Ok(r.unwrap_or(Value::Bool(true)))
}

fn proto_some(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let r = iterate(vm, a, "some", false, |_, _, _, b| b.then_some(Value::Bool(true)))?;
    Ok(r.unwrap_or(Value::Bool(false)))
}

fn proto_for_each(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    iterate(vm, a, "forEach", false, |_, _, _, _| None)?;
    Ok(Value::Undefined)
}

fn proto_find(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let r = iterate(vm, a, "find", false, |_, v, _, b| b.then_some(v))?;
    Ok(r.unwrap_or(Value::Undefined))
}

fn proto_find_index(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let r = iterate(vm, a, "findIndex", false, |_, _, k, b| b.then_some(Value::Number(k as f64)))?;
    Ok(r.unwrap_or(Value::Number(-1.0)))
}

fn proto_find_last(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let r = iterate(vm, a, "findLast", true, |_, v, _, b| b.then_some(v))?;
    Ok(r.unwrap_or(Value::Undefined))
}

fn proto_find_last_index(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let r = iterate(vm, a, "findLastIndex", true, |_, _, k, b| {
        b.then_some(Value::Number(k as f64))
    })?;
    Ok(r.unwrap_or(Value::Number(-1.0)))
}

fn proto_fill(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = validate_typed_array(vm, a.this, "fill")?;
    let kind = ta_kind(vm, o);
    let value = if kind.is_bigint() {
        let b = vm.to_bigint(a.arg(0))?;
        Value::BigInt(b)
    } else {
        Value::Number(vm.to_number(a.arg(0))?)
    };
    let lenf = len as f64;
    let start = relative_index(vm, a.arg(1), lenf, 0.0)?;
    let end = relative_index(vm, a.arg(2), lenf, lenf)?;
    let Some(len) = ta_length(vm, o) else {
        return vm.type_error("%TypedArray%.prototype.fill: typed array is detached or out of bounds");
    };
    let end = end.min(len as f64);
    let c = converted(vm, value);
    let mut k = start as usize;
    while (k as f64) < end {
        ta_store(vm, o, k, c);
        k += 1;
    }
    Ok(Value::Object(o))
}

fn proto_filter(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = validate_typed_array(vm, a.this, "filter")?;
    let cb = callable_arg(vm, a.arg(0), "filter")?;
    let this_arg = a.arg(1);
    let mut kept = Vec::new();
    for k in 0..len {
        let v = elem(vm, o, k);
        let r = vm.call_value(cb, this_arg, &[v, Value::Number(k as f64), Value::Object(o)])?;
        if vm.to_boolean(r) {
            kept.push(v);
        }
    }
    let n = kept.len();
    let result = typed_array_species_create(vm, o, &[Value::Number(n as f64)])?;
    for (i, v) in kept.into_iter().enumerate() {
        set_elem(vm, result, i, v)?;
    }
    Ok(Value::Object(result))
}

fn proto_includes(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = validate_typed_array(vm, a.this, "includes")?;
    if len == 0 {
        return Ok(Value::Bool(false));
    }
    let lenf = len as f64;
    let n = vm.to_integer_or_infinity(a.arg(1))?;
    if n == f64::INFINITY {
        return Ok(Value::Bool(false));
    }
    let mut k = if n >= 0.0 {
        n
    } else {
        (lenf + n).max(0.0)
    };
    let search = a.arg(0);
    while k < lenf {
        let v = elem(vm, o, k as usize);
        if vm.same_value_zero(v, search) {
            return Ok(Value::Bool(true));
        }
        k += 1.0;
    }
    Ok(Value::Bool(false))
}

fn proto_index_of(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = validate_typed_array(vm, a.this, "indexOf")?;
    if len == 0 {
        return Ok(Value::Number(-1.0));
    }
    let lenf = len as f64;
    let n = vm.to_integer_or_infinity(a.arg(1))?;
    if n == f64::INFINITY {
        return Ok(Value::Number(-1.0));
    }
    let mut k = if n >= 0.0 {
        n
    } else {
        (lenf + n).max(0.0)
    };
    let search = a.arg(0);
    while k < lenf {
        if let Some(i) = valid_index(vm, o, k) {
            let v = ta_get_element(vm, o, i);
            if vm.strict_equals(v, search) {
                return Ok(Value::Number(k));
            }
        }
        k += 1.0;
    }
    Ok(Value::Number(-1.0))
}

fn proto_last_index_of(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = validate_typed_array(vm, a.this, "lastIndexOf")?;
    if len == 0 {
        return Ok(Value::Number(-1.0));
    }
    let lenf = len as f64;
    let n = if a.args.len() > 1 {
        vm.to_integer_or_infinity(a.arg(1))?
    } else {
        lenf - 1.0
    };
    if n == f64::NEG_INFINITY {
        return Ok(Value::Number(-1.0));
    }
    let mut k = if n >= 0.0 {
        n.min(lenf - 1.0)
    } else {
        lenf + n
    };
    let search = a.arg(0);
    while k >= 0.0 {
        if let Some(i) = valid_index(vm, o, k) {
            let v = ta_get_element(vm, o, i);
            if vm.strict_equals(v, search) {
                return Ok(Value::Number(k));
            }
        }
        k -= 1.0;
    }
    Ok(Value::Number(-1.0))
}

fn proto_join(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = validate_typed_array(vm, a.this, "join")?;
    let sep_arg = a.arg(0);
    let sep: Vec<u16> = if sep_arg.is_undefined() {
        vec![b',' as u16]
    } else {
        let s = vm.to_string(sep_arg)?;
        vm.heap.units(s).to_vec()
    };
    if len > 1 && (len - 1).saturating_mul(sep.len()) > vm.max_string_length() {
        return vm.string_too_long();
    }
    let mut b = StrBuilder::new();
    for k in 0..len {
        if k > 0 {
            b.push_slice(&sep);
        }
        let v = elem(vm, o, k);
        if !v.is_undefined() {
            let s = vm.to_string(v)?;
            let u = vm.heap.units(s);
            b.push_units(&u);
            vm.check_string_len(b.len())?;
        }
    }
    vm.string_from_builder(b)
}

fn proto_map(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = validate_typed_array(vm, a.this, "map")?;
    let cb = callable_arg(vm, a.arg(0), "map")?;
    let this_arg = a.arg(1);
    let result = typed_array_species_create(vm, o, &[Value::Number(len as f64)])?;
    for k in 0..len {
        let v = elem(vm, o, k);
        let mapped = vm.call_value(cb, this_arg, &[v, Value::Number(k as f64), Value::Object(o)])?;
        set_elem(vm, result, k, mapped)?;
    }
    Ok(Value::Object(result))
}

fn reduce_impl(vm: &mut Vm, a: &CallArgs, method: &str, right: bool) -> JsResult<Value> {
    let (o, len) = validate_typed_array(vm, a.this, method)?;
    let cb = callable_arg(vm, a.arg(0), method)?;
    let mut indices: Box<dyn Iterator<Item = usize>> = if right {
        Box::new((0..len).rev())
    } else {
        Box::new(0..len)
    };
    let mut acc = if a.args.len() > 1 {
        a.arg(1)
    } else {
        match indices.next() {
            Some(k) => elem(vm, o, k),
            None => {
                return vm.type_error(&format!(
                    "%TypedArray%.prototype.{method} of empty array with no initial value"
                ))
            }
        }
    };
    for k in indices {
        let v = elem(vm, o, k);
        acc = vm.call_value(cb, Value::Undefined, &[acc, v, Value::Number(k as f64), Value::Object(o)])?;
    }
    Ok(acc)
}

fn proto_reduce(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    reduce_impl(vm, a, "reduce", false)
}

fn proto_reduce_right(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    reduce_impl(vm, a, "reduceRight", true)
}

fn proto_reverse(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = validate_typed_array(vm, a.this, "reverse")?;
    let (buffer, kind, offset) = {
        let t = ta_data(vm, o).unwrap();
        (t.buffer, t.kind, t.byte_offset)
    };
    let size = kind.size();
    if let Class::ArrayBuffer(b) = &mut vm.heap.obj_mut(buffer).class {
        let region = &mut b.bytes[offset..offset + len * size];
        let mut lo = 0;
        let mut hi = len;
        while lo + 1 < hi {
            hi -= 1;
            let (left, right) = region.split_at_mut(hi * size);
            left[lo * size..(lo + 1) * size].swap_with_slice(&mut right[..size]);
            lo += 1;
        }
    }
    Ok(Value::Object(o))
}

fn proto_set(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let target = require_ta(vm, a.this, "set")?;
    let source = a.arg(0);
    let target_offset = vm.to_integer_or_infinity(a.arg(1))?;
    if target_offset < 0.0 {
        return vm.range_error("offset is out of bounds");
    }
    if let Value::Object(src) = source {
        if ta_data(vm, src).is_some() {
            set_from_typed_array(vm, target, target_offset, src)?;
            return Ok(Value::Undefined);
        }
    }
    set_from_array_like(vm, target, target_offset, source)?;
    Ok(Value::Undefined)
}

fn set_from_typed_array(vm: &mut Vm, target: ObjId, target_offset: f64, src: ObjId) -> JsResult<()> {
    let Some(target_len) = ta_length(vm, target) else {
        return vm.type_error("%TypedArray%.prototype.set: target is detached or out of bounds");
    };
    let Some(src_len) = ta_length(vm, src) else {
        return vm.type_error("%TypedArray%.prototype.set: source is detached or out of bounds");
    };
    let (t_buf, t_kind, t_off) = {
        let t = ta_data(vm, target).unwrap();
        (t.buffer, t.kind, t.byte_offset)
    };
    let (s_buf, s_kind, s_off) = {
        let t = ta_data(vm, src).unwrap();
        (t.buffer, t.kind, t.byte_offset)
    };
    if t_kind.is_bigint() != s_kind.is_bigint() {
        return vm.type_error("Cannot mix BigInt and other types");
    }
    if target_offset == f64::INFINITY || src_len as f64 + target_offset > target_len as f64 {
        return vm.range_error("offset is out of bounds");
    }
    let target_offset = target_offset as usize;
    let s_size = s_kind.size();
    let t_size = t_kind.size();
    // Snapshot the source bytes: this also covers overlapping views of one buffer.
    let src_bytes: Vec<u8> = buffer_data(vm, s_buf).unwrap().bytes[s_off..s_off + src_len * s_size].to_vec();
    if t_kind == s_kind {
        let at = t_off + target_offset * t_size;
        if let Class::ArrayBuffer(b) = &mut vm.heap.obj_mut(t_buf).class {
            b.bytes[at..at + src_bytes.len()].copy_from_slice(&src_bytes);
        }
    } else {
        for i in 0..src_len {
            let raw = super::typedarray::read_element(&src_bytes[i * s_size..(i + 1) * s_size], s_kind);
            let v = match raw {
                Converted::Num(n) => Value::Number(n),
                Converted::Big(b) => vm.bigint_value(num_bigint::BigInt::from(b)),
            };
            let c = converted(vm, v);
            ta_store(vm, target, target_offset + i, c);
        }
    }
    Ok(())
}

fn set_from_array_like(vm: &mut Vm, target: ObjId, target_offset: f64, source: Value) -> JsResult<()> {
    let Some(target_len) = ta_length(vm, target) else {
        return vm.type_error("%TypedArray%.prototype.set: target is detached or out of bounds");
    };
    let src = vm.to_object(source)?;
    let src_len = vm.length_of_array_like(src)?;
    if target_offset == f64::INFINITY || src_len + target_offset > target_len as f64 {
        return vm.range_error("offset is out of bounds");
    }
    let target_offset = target_offset as usize;
    let mut k = 0usize;
    while (k as f64) < src_len {
        let key = vm.number_key(k as f64);
        let v = vm.get(src, key)?;
        ta_set_element(vm, target, target_offset + k, v)?;
        k += 1;
    }
    Ok(())
}

fn proto_slice(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = validate_typed_array(vm, a.this, "slice")?;
    let lenf = len as f64;
    let start = relative_index(vm, a.arg(0), lenf, 0.0)?;
    let mut end = relative_index(vm, a.arg(1), lenf, lenf)?;
    let count = (end - start).max(0.0);
    let result = typed_array_species_create(vm, o, &[Value::Number(count)])?;
    if count > 0.0 {
        let Some(len) = ta_length(vm, o) else {
            return vm.type_error("%TypedArray%.prototype.slice: typed array is detached or out of bounds");
        };
        end = end.min(len as f64);
        let count = (end - start).max(0.0) as usize;
        let src_kind = ta_kind(vm, o);
        let dst_kind = ta_kind(vm, result);
        if src_kind == dst_kind {
            let size = src_kind.size();
            let (s_buf, s_off) = {
                let t = ta_data(vm, o).unwrap();
                (t.buffer, t.byte_offset)
            };
            let (d_buf, d_off) = {
                let t = ta_data(vm, result).unwrap();
                (t.buffer, t.byte_offset)
            };
            let from = s_off + start as usize * size;
            let d_len = ta_length(vm, result).unwrap_or(0) * size;
            let n = (count * size).min(d_len);
            // The spec copies byte by byte in order; with overlapping views
            // of one buffer that is observable, so mirror it exactly.
            if let Class::ArrayBuffer(b) = &mut vm.heap.obj_mut(d_buf).class {
                if s_buf == d_buf {
                    for i in 0..n {
                        b.bytes[d_off + i] = b.bytes[from + i];
                    }
                }
            }
            if s_buf != d_buf && n > 0 {
                let bytes: Vec<u8> = buffer_data(vm, s_buf).unwrap().bytes[from..from + n].to_vec();
                if let Class::ArrayBuffer(b) = &mut vm.heap.obj_mut(d_buf).class {
                    b.bytes[d_off..d_off + n].copy_from_slice(&bytes);
                }
            }
        } else {
            let mut n = 0;
            let mut k = start as usize;
            while (k as f64) < end {
                let v = elem(vm, o, k);
                set_elem(vm, result, n, v)?;
                k += 1;
                n += 1;
            }
        }
    }
    Ok(Value::Object(result))
}

fn compare_numeric(x: Value, y: Value, vm: &Vm) -> Ordering {
    match (x, y) {
        (Value::Number(a), Value::Number(b)) => {
            if a.is_nan() && b.is_nan() {
                return Ordering::Equal;
            }
            if a.is_nan() {
                return Ordering::Greater;
            }
            if b.is_nan() {
                return Ordering::Less;
            }
            if a < b {
                return Ordering::Less;
            }
            if a > b {
                return Ordering::Greater;
            }
            if a == 0.0 && b == 0.0 {
                return match (a.is_sign_negative(), b.is_sign_negative()) {
                    (true, false) => Ordering::Less,
                    (false, true) => Ordering::Greater,
                    _ => Ordering::Equal,
                };
            }
            Ordering::Equal
        }
        (Value::BigInt(a), Value::BigInt(b)) => vm.heap.bigint(a).0.cmp(&vm.heap.bigint(b).0),
        _ => Ordering::Equal,
    }
}

fn sort_elements(vm: &mut Vm, items: Vec<Value>, comparefn: Value) -> JsResult<Vec<Value>> {
    super::array::sort_values(vm, items, |vm, x, y| {
        if comparefn.is_undefined() {
            return Ok(compare_numeric(x, y, vm));
        }
        let r = vm.call_value(comparefn, Value::Undefined, &[x, y])?;
        let n = vm.to_number(r)?;
        Ok(if n < 0.0 {
            Ordering::Less
        } else if n > 0.0 {
            Ordering::Greater
        } else {
            Ordering::Equal
        })
    })
}

fn proto_sort(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let comparefn = a.arg(0);
    if !comparefn.is_undefined() && !vm.is_callable(comparefn) {
        return vm.type_error("The comparison function must be either a function or undefined");
    }
    let (o, len) = validate_typed_array(vm, a.this, "sort")?;
    let items: Vec<Value> = (0..len).map(|k| ta_get_element(vm, o, k)).collect();
    let sorted = sort_elements(vm, items, comparefn)?;
    for (j, v) in sorted.into_iter().enumerate() {
        if valid_index(vm, o, j as f64).is_some() {
            let c = converted(vm, v);
            ta_store(vm, o, j, c);
        }
    }
    Ok(Value::Object(o))
}

fn proto_subarray(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = require_ta(vm, a.this, "subarray")?;
    let (buffer, kind, offset, tracking) = {
        let t = ta_data(vm, o).unwrap();
        (t.buffer, t.kind, t.byte_offset, t.length.is_none())
    };
    let src_len = ta_length(vm, o).unwrap_or(0) as f64;
    let start = relative_index(vm, a.arg(0), src_len, 0.0)?;
    let size = kind.size() as f64;
    let begin_byte_offset = offset as f64 + start * size;
    if tracking && a.arg(1).is_undefined() {
        let args = [Value::Object(buffer), Value::Number(begin_byte_offset)];
        return Ok(Value::Object(typed_array_species_create(vm, o, &args)?));
    }
    let end = relative_index(vm, a.arg(1), src_len, src_len)?;
    let new_len = (end - start).max(0.0);
    let args = [
        Value::Object(buffer),
        Value::Number(begin_byte_offset),
        Value::Number(new_len),
    ];
    Ok(Value::Object(typed_array_species_create(vm, o, &args)?))
}

fn proto_to_locale_string(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = validate_typed_array(vm, a.this, "toLocaleString")?;
    // ECMA-402 forwards locales and options to each element; ECMA-262
    // alone passes no arguments.
    #[cfg(feature = "intl")]
    let args = [a.arg(0), a.arg(1)];
    #[cfg(not(feature = "intl"))]
    let args: [Value; 0] = [];
    let mut b = StrBuilder::new();
    for k in 0..len {
        if k > 0 {
            b.push(b',' as u16);
        }
        let v = elem(vm, o, k);
        if !v.is_nullish() {
            let r = vm.invoke_str(v, "toLocaleString", &args)?;
            let s = vm.to_string(r)?;
            let u = vm.heap.units(s);
            b.push_units(&u);
            vm.check_string_len(b.len())?;
        }
    }
    vm.string_from_builder(b)
}

fn proto_to_reversed(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = validate_typed_array(vm, a.this, "toReversed")?;
    let result = typed_array_create_same_type(vm, o, len as f64)?;
    for k in 0..len {
        let v = elem(vm, o, len - 1 - k);
        set_elem(vm, result, k, v)?;
    }
    Ok(Value::Object(result))
}

fn proto_to_sorted(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let comparefn = a.arg(0);
    if !comparefn.is_undefined() && !vm.is_callable(comparefn) {
        return vm.type_error("The comparison function must be either a function or undefined");
    }
    let (o, len) = validate_typed_array(vm, a.this, "toSorted")?;
    let result = typed_array_create_same_type(vm, o, len as f64)?;
    let items: Vec<Value> = (0..len).map(|k| ta_get_element(vm, o, k)).collect();
    let sorted = sort_elements(vm, items, comparefn)?;
    for (j, v) in sorted.into_iter().enumerate() {
        set_elem(vm, result, j, v)?;
    }
    Ok(Value::Object(result))
}

fn proto_with(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, len) = validate_typed_array(vm, a.this, "with")?;
    let rel = vm.to_integer_or_infinity(a.arg(0))?;
    let actual = if rel >= 0.0 { rel } else { len as f64 + rel };
    let kind = ta_kind(vm, o);
    let value = if kind.is_bigint() {
        Value::BigInt(vm.to_bigint(a.arg(1))?)
    } else {
        Value::Number(vm.to_number(a.arg(1))?)
    };
    if valid_index(vm, o, actual).is_none() {
        return vm.range_error("Invalid typed array index");
    }
    let result = typed_array_create_same_type(vm, o, len as f64)?;
    for k in 0..len {
        let v = if k as f64 == actual { value } else { elem(vm, o, k) };
        set_elem(vm, result, k, v)?;
    }
    Ok(Value::Object(result))
}
