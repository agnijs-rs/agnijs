//! ArrayBuffer, SharedArrayBuffer and DataView (ES2025 §25.1–25.3).
//! Buffers are plain `Vec<u8>`s; the engine runs one agent per host
//! thread, so a SharedArrayBuffer is an ArrayBuffer that cannot be
//! detached and grows instead of resizing.

use agnijs_engine_api::RealmId;

use super::typedarray::{
    read_element, write_element, BufferData, Converted, DataViewData, ElementKind,
};
use crate::object::{CallArgs, Class, ObjectData};
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, PropKey, Value};
use crate::vm::{JsResult, Vm};

/// The largest buffer the engine will allocate. Beyond it `new
/// ArrayBuffer` throws a RangeError like an allocation failure.
pub const MAX_BUFFER_BYTES: f64 = 4_294_967_295.0;

// ---- buffer helpers ----------------------------------------------------------------------

pub fn buffer_data(vm: &Vm, o: ObjId) -> Option<&BufferData> {
    match &vm.heap.obj(o).class {
        Class::ArrayBuffer(b) => Some(b),
        _ => None,
    }
}

pub fn is_detached(vm: &Vm, buffer: ObjId) -> bool {
    buffer_data(vm, buffer).map(|b| b.detached).unwrap_or(false)
}

pub fn buffer_byte_length(vm: &Vm, buffer: ObjId) -> usize {
    buffer_data(vm, buffer).map(|b| b.bytes.len()).unwrap_or(0)
}

pub fn is_resizable(vm: &Vm, buffer: ObjId) -> bool {
    buffer_data(vm, buffer)
        .map(|b| b.max_byte_length.is_some())
        .unwrap_or(false)
}

pub fn is_shared(vm: &Vm, buffer: ObjId) -> bool {
    buffer_data(vm, buffer).map(|b| b.shared).unwrap_or(false)
}

fn zeroed(vm: &mut Vm, len: f64) -> JsResult<Vec<u8>> {
    if len > MAX_BUFFER_BYTES {
        return vm.range_error("Array buffer allocation failed");
    }
    let n = len as usize;
    let mut v = Vec::new();
    if v.try_reserve_exact(n).is_err() {
        return vm.range_error("Array buffer allocation failed");
    }
    v.resize(n, 0);
    Ok(v)
}

/// AllocateArrayBuffer / AllocateSharedArrayBuffer with the prototype
/// already resolved.
pub fn allocate_buffer(
    vm: &mut Vm,
    proto: ObjId,
    len: f64,
    max: Option<f64>,
    shared: bool,
) -> JsResult<ObjId> {
    if let Some(m) = max {
        if m > MAX_BUFFER_BYTES {
            return vm.range_error("Array buffer allocation failed");
        }
    }
    let bytes = zeroed(vm, len)?;
    let data = BufferData {
        bytes,
        detached: false,
        shared,
        max_byte_length: max.map(|m| m as usize),
    };
    Ok(vm.alloc(ObjectData::new(Some(proto), Class::ArrayBuffer(Box::new(data)))))
}

/// A fresh fixed-length %ArrayBuffer% of `len` bytes in the current realm.
pub fn new_array_buffer(vm: &mut Vm, len: f64) -> JsResult<ObjId> {
    let proto = vm.intrinsic(Intrinsic::ArrayBufferPrototype);
    allocate_buffer(vm, proto, len, None, false)
}

fn this_buffer(vm: &mut Vm, this: Value, shared: bool, method: &str) -> JsResult<ObjId> {
    if let Value::Object(o) = this {
        if let Class::ArrayBuffer(b) = &vm.heap.obj(o).class {
            if b.shared == shared {
                return Ok(o);
            }
        }
    }
    let ctor = if shared { "SharedArrayBuffer" } else { "ArrayBuffer" };
    vm.type_error(&format!(
        "{ctor}.prototype.{method} called on incompatible receiver"
    ))
}

/// GetArrayBufferMaxByteLengthOption.
fn max_byte_length_option(vm: &mut Vm, options: Value) -> JsResult<Option<f64>> {
    let Value::Object(o) = options else {
        return Ok(None);
    };
    let key = PropKey::Str(vm.names.maxByteLength);
    let v = vm.get(o, key)?;
    if v.is_undefined() {
        return Ok(None);
    }
    Ok(Some(vm.to_index(v)?))
}

// ---- install -----------------------------------------------------------------------------

pub fn install(vm: &mut Vm, realm: RealmId) {
    // ArrayBuffer
    let proto = vm.new_object();
    let ctor = super::install_ctor(
        vm,
        realm,
        "ArrayBuffer",
        1,
        array_buffer_constructor,
        proto,
        Intrinsic::ArrayBuffer,
        Intrinsic::ArrayBufferPrototype,
    );
    vm.method(ctor, "isView", 1, array_buffer_is_view);
    super::species_accessor(vm, ctor);
    vm.accessor(proto, "byteLength", ab_byte_length, None);
    vm.accessor(proto, "detached", ab_detached, None);
    vm.accessor(proto, "maxByteLength", ab_max_byte_length, None);
    vm.accessor(proto, "resizable", ab_resizable, None);
    vm.method(proto, "resize", 1, ab_resize);
    vm.method(proto, "slice", 2, ab_slice);
    vm.method(proto, "transfer", 0, ab_transfer);
    vm.method(proto, "transferToFixedLength", 0, ab_transfer_to_fixed_length);
    super::to_string_tag(vm, proto, "ArrayBuffer");

    // SharedArrayBuffer
    let proto = vm.new_object();
    let ctor = super::install_ctor(
        vm,
        realm,
        "SharedArrayBuffer",
        1,
        shared_array_buffer_constructor,
        proto,
        Intrinsic::SharedArrayBuffer,
        Intrinsic::SharedArrayBufferPrototype,
    );
    super::species_accessor(vm, ctor);
    vm.accessor(proto, "byteLength", sab_byte_length, None);
    vm.accessor(proto, "growable", sab_growable, None);
    vm.accessor(proto, "maxByteLength", sab_max_byte_length, None);
    vm.method(proto, "grow", 1, sab_grow);
    vm.method(proto, "slice", 2, sab_slice);
    super::to_string_tag(vm, proto, "SharedArrayBuffer");

    // DataView
    let proto = vm.new_object();
    super::install_ctor(
        vm,
        realm,
        "DataView",
        1,
        data_view_constructor,
        proto,
        Intrinsic::DataView,
        Intrinsic::DataViewPrototype,
    );
    vm.accessor(proto, "buffer", dv_buffer, None);
    vm.accessor(proto, "byteLength", dv_byte_length, None);
    vm.accessor(proto, "byteOffset", dv_byte_offset, None);
    for (get, set, kind) in [
        ("getBigInt64", "setBigInt64", ElementKind::BigInt64),
        ("getBigUint64", "setBigUint64", ElementKind::BigUint64),
        ("getFloat16", "setFloat16", ElementKind::Float16),
        ("getFloat32", "setFloat32", ElementKind::Float32),
        ("getFloat64", "setFloat64", ElementKind::Float64),
        ("getInt8", "setInt8", ElementKind::Int8),
        ("getInt16", "setInt16", ElementKind::Int16),
        ("getInt32", "setInt32", ElementKind::Int32),
        ("getUint8", "setUint8", ElementKind::Uint8),
        ("getUint16", "setUint16", ElementKind::Uint16),
        ("getUint32", "setUint32", ElementKind::Uint32),
    ] {
        let g = vm.method(proto, get, 1, dv_get);
        let s = vm.method(proto, set, 2, dv_set);
        let tag = Value::Number(kind_tag(kind) as f64);
        vm.set_native_slot(g, 0, tag);
        vm.set_native_slot(s, 0, tag);
    }
    super::to_string_tag(vm, proto, "DataView");
    let _ = Attrs::NONE;
}

fn kind_tag(kind: ElementKind) -> u8 {
    match kind {
        ElementKind::Int8 => 0,
        ElementKind::Uint8 => 1,
        ElementKind::Uint8Clamped => 2,
        ElementKind::Int16 => 3,
        ElementKind::Uint16 => 4,
        ElementKind::Int32 => 5,
        ElementKind::Uint32 => 6,
        ElementKind::Float16 => 7,
        ElementKind::Float32 => 8,
        ElementKind::Float64 => 9,
        ElementKind::BigInt64 => 10,
        ElementKind::BigUint64 => 11,
    }
}

pub fn kind_from_tag(tag: u8) -> ElementKind {
    match tag {
        0 => ElementKind::Int8,
        1 => ElementKind::Uint8,
        2 => ElementKind::Uint8Clamped,
        3 => ElementKind::Int16,
        4 => ElementKind::Uint16,
        5 => ElementKind::Int32,
        6 => ElementKind::Uint32,
        7 => ElementKind::Float16,
        8 => ElementKind::Float32,
        9 => ElementKind::Float64,
        10 => ElementKind::BigInt64,
        _ => ElementKind::BigUint64,
    }
}

// ---- ArrayBuffer -------------------------------------------------------------------------

fn array_buffer_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if a.new_target.is_undefined() {
        return vm.type_error("Constructor ArrayBuffer requires 'new'");
    }
    let byte_length = vm.to_index(a.arg(0))?;
    let max = max_byte_length_option(vm, a.arg(1))?;
    if max.is_some_and(|m| byte_length > m) {
        return vm.range_error("byteLength exceeds maxByteLength");
    }
    let proto = vm.get_prototype_from_constructor(a.new_target, Intrinsic::ArrayBufferPrototype)?;
    Ok(Value::Object(allocate_buffer(vm, proto, byte_length, max, false)?))
}

fn array_buffer_is_view(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Value::Object(o) = a.arg(0) else {
        return Ok(Value::Bool(false));
    };
    Ok(Value::Bool(matches!(
        vm.heap.obj(o).class,
        Class::TypedArray(_) | Class::DataView(_)
    )))
}

fn ab_byte_length(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_buffer(vm, a.this, false, "byteLength")?;
    Ok(Value::Number(buffer_byte_length(vm, o) as f64))
}

fn ab_detached(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_buffer(vm, a.this, false, "detached")?;
    Ok(Value::Bool(is_detached(vm, o)))
}

fn ab_max_byte_length(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_buffer(vm, a.this, false, "maxByteLength")?;
    let b = buffer_data(vm, o).unwrap();
    if b.detached {
        return Ok(Value::Number(0.0));
    }
    Ok(Value::Number(b.max_byte_length.unwrap_or(b.bytes.len()) as f64))
}

fn ab_resizable(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_buffer(vm, a.this, false, "resizable")?;
    Ok(Value::Bool(is_resizable(vm, o)))
}

fn ab_resize(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_buffer(vm, a.this, false, "resize")?;
    if !is_resizable(vm, o) {
        return vm.type_error("ArrayBuffer.prototype.resize called on a fixed-length buffer");
    }
    let new_len = vm.to_index(a.arg(0))?;
    if is_detached(vm, o) {
        return vm.type_error("ArrayBuffer is detached");
    }
    let max = buffer_data(vm, o).unwrap().max_byte_length.unwrap();
    if new_len > max as f64 {
        return vm.range_error("new length exceeds maxByteLength");
    }
    if let Class::ArrayBuffer(b) = &mut vm.heap.obj_mut(o).class {
        b.bytes.resize(new_len as usize, 0);
    }
    Ok(Value::Undefined)
}

fn slice_impl(vm: &mut Vm, a: &CallArgs, shared: bool) -> JsResult<Value> {
    let o = this_buffer(vm, a.this, shared, "slice")?;
    if is_detached(vm, o) {
        return vm.type_error("ArrayBuffer is detached");
    }
    let len = buffer_byte_length(vm, o) as f64;
    let rel_start = vm.to_integer_or_infinity(a.arg(0))?;
    let first = if rel_start < 0.0 {
        (len + rel_start).max(0.0)
    } else {
        rel_start.min(len)
    };
    let end = a.arg(1);
    let rel_end = if end.is_undefined() {
        len
    } else {
        vm.to_integer_or_infinity(end)?
    };
    let fin = if rel_end < 0.0 {
        (len + rel_end).max(0.0)
    } else {
        rel_end.min(len)
    };
    let new_len = (fin - first).max(0.0);
    let default = if shared {
        Intrinsic::SharedArrayBuffer
    } else {
        Intrinsic::ArrayBuffer
    };
    let ctor = vm.species_constructor(o, default)?;
    let new = vm.construct_value(ctor, &[Value::Number(new_len)], ctor)?;
    let Value::Object(n) = new else {
        return vm.type_error("species constructor did not return an object");
    };
    match buffer_data(vm, n) {
        Some(b) if b.shared == shared => {}
        _ => {
            let what = if shared { "a SharedArrayBuffer" } else { "an ArrayBuffer" };
            return vm.type_error(&format!("species constructor did not return {what}"));
        }
    }
    if !shared && is_detached(vm, n) {
        return vm.type_error("species constructor returned a detached ArrayBuffer");
    }
    if n == o {
        return vm.type_error("species constructor returned the same buffer");
    }
    if (buffer_byte_length(vm, n) as f64) < new_len {
        return vm.type_error("species constructor returned a buffer that is too small");
    }
    if !shared && is_detached(vm, o) {
        return vm.type_error("ArrayBuffer is detached");
    }
    let cur_len = buffer_byte_length(vm, o);
    let first = first as usize;
    let count = (new_len as usize).min(cur_len.saturating_sub(first));
    let src: Vec<u8> = buffer_data(vm, o).unwrap().bytes[first..first + count].to_vec();
    if let Class::ArrayBuffer(b) = &mut vm.heap.obj_mut(n).class {
        b.bytes[..count].copy_from_slice(&src);
    }
    Ok(new)
}

fn ab_slice(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    slice_impl(vm, a, false)
}

fn transfer_impl(vm: &mut Vm, a: &CallArgs, preserve_resizability: bool, method: &str) -> JsResult<Value> {
    let o = this_buffer(vm, a.this, false, method)?;
    let new_len = if a.arg(0).is_undefined() {
        buffer_byte_length(vm, o) as f64
    } else {
        vm.to_index(a.arg(0))?
    };
    if is_detached(vm, o) {
        return vm.type_error("ArrayBuffer is detached");
    }
    let max = if preserve_resizability {
        buffer_data(vm, o).unwrap().max_byte_length.map(|m| m as f64)
    } else {
        None
    };
    let proto = vm.intrinsic(Intrinsic::ArrayBufferPrototype);
    let new = allocate_buffer(vm, proto, new_len, max, false)?;
    let old = match &mut vm.heap.obj_mut(o).class {
        Class::ArrayBuffer(b) => {
            b.detached = true;
            std::mem::take(&mut b.bytes)
        }
        _ => unreachable!(),
    };
    if let Class::ArrayBuffer(b) = &mut vm.heap.obj_mut(new).class {
        let n = old.len().min(b.bytes.len());
        b.bytes[..n].copy_from_slice(&old[..n]);
    }
    Ok(Value::Object(new))
}

fn ab_transfer(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    transfer_impl(vm, a, true, "transfer")
}

fn ab_transfer_to_fixed_length(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    transfer_impl(vm, a, false, "transferToFixedLength")
}

// ---- SharedArrayBuffer -------------------------------------------------------------------

fn shared_array_buffer_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if a.new_target.is_undefined() {
        return vm.type_error("Constructor SharedArrayBuffer requires 'new'");
    }
    let byte_length = vm.to_index(a.arg(0))?;
    let max = max_byte_length_option(vm, a.arg(1))?;
    if max.is_some_and(|m| byte_length > m) {
        return vm.range_error("byteLength exceeds maxByteLength");
    }
    let proto =
        vm.get_prototype_from_constructor(a.new_target, Intrinsic::SharedArrayBufferPrototype)?;
    Ok(Value::Object(allocate_buffer(vm, proto, byte_length, max, true)?))
}

fn sab_byte_length(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_buffer(vm, a.this, true, "byteLength")?;
    Ok(Value::Number(buffer_byte_length(vm, o) as f64))
}

fn sab_growable(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_buffer(vm, a.this, true, "growable")?;
    Ok(Value::Bool(is_resizable(vm, o)))
}

fn sab_max_byte_length(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_buffer(vm, a.this, true, "maxByteLength")?;
    let b = buffer_data(vm, o).unwrap();
    Ok(Value::Number(b.max_byte_length.unwrap_or(b.bytes.len()) as f64))
}

fn sab_grow(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_buffer(vm, a.this, true, "grow")?;
    if !is_resizable(vm, o) {
        return vm.type_error("SharedArrayBuffer.prototype.grow called on a fixed-length buffer");
    }
    let new_len = vm.to_index(a.arg(0))?;
    let b = buffer_data(vm, o).unwrap();
    let (cur, max) = (b.bytes.len(), b.max_byte_length.unwrap());
    if new_len > max as f64 {
        return vm.range_error("new length exceeds maxByteLength");
    }
    if new_len < cur as f64 {
        return vm.range_error("a SharedArrayBuffer cannot shrink");
    }
    if let Class::ArrayBuffer(b) = &mut vm.heap.obj_mut(o).class {
        b.bytes.resize(new_len as usize, 0);
    }
    Ok(Value::Undefined)
}

fn sab_slice(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    slice_impl(vm, a, true)
}

// ---- DataView ----------------------------------------------------------------------------

fn data_view_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if a.new_target.is_undefined() {
        return vm.type_error("Constructor DataView requires 'new'");
    }
    let Value::Object(buffer) = a.arg(0) else {
        return vm.type_error("First argument to DataView constructor must be an ArrayBuffer");
    };
    if buffer_data(vm, buffer).is_none() {
        return vm.type_error("First argument to DataView constructor must be an ArrayBuffer");
    }
    let offset = vm.to_index(a.arg(1))?;
    if is_detached(vm, buffer) {
        return vm.type_error("ArrayBuffer is detached");
    }
    let mut buf_len = buffer_byte_length(vm, buffer) as f64;
    if offset > buf_len {
        return vm.range_error("Start offset is outside the bounds of the buffer");
    }
    let length_tracking = a.arg(2).is_undefined() && is_resizable(vm, buffer);
    let view_len: Option<f64> = if a.arg(2).is_undefined() {
        if length_tracking {
            None
        } else {
            Some(buf_len - offset)
        }
    } else {
        let l = vm.to_index(a.arg(2))?;
        if offset + l > buf_len {
            return vm.range_error("Invalid DataView length");
        }
        Some(l)
    };
    let proto = vm.get_prototype_from_constructor(a.new_target, Intrinsic::DataViewPrototype)?;
    if is_detached(vm, buffer) {
        return vm.type_error("ArrayBuffer is detached");
    }
    buf_len = buffer_byte_length(vm, buffer) as f64;
    if offset > buf_len {
        return vm.range_error("Start offset is outside the bounds of the buffer");
    }
    if let Some(l) = view_len {
        if offset + l > buf_len {
            return vm.range_error("Invalid DataView length");
        }
    }
    let data = DataViewData {
        buffer,
        byte_offset: offset as usize,
        byte_length: view_len.map(|l| l as usize),
    };
    Ok(Value::Object(vm.alloc(ObjectData::new(
        Some(proto),
        Class::DataView(Box::new(data)),
    ))))
}

fn this_view(vm: &mut Vm, this: Value, method: &str) -> JsResult<(ObjId, ObjId, usize, Option<usize>)> {
    if let Value::Object(o) = this {
        if let Class::DataView(d) = &vm.heap.obj(o).class {
            return Ok((o, d.buffer, d.byte_offset, d.byte_length));
        }
    }
    vm.type_error(&format!("DataView.prototype.{method} called on incompatible receiver"))
}

/// GetViewByteLength, or `None` when the view is out of bounds.
fn view_byte_length(vm: &Vm, buffer: ObjId, offset: usize, len: Option<usize>) -> Option<usize> {
    let b = buffer_data(vm, buffer)?;
    if b.detached {
        return None;
    }
    let buf_len = b.bytes.len();
    match len {
        Some(l) => {
            if offset + l > buf_len {
                None
            } else {
                Some(l)
            }
        }
        None => {
            if offset > buf_len {
                None
            } else {
                Some(buf_len - offset)
            }
        }
    }
}

fn dv_buffer(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (_, buffer, _, _) = this_view(vm, a.this, "buffer")?;
    Ok(Value::Object(buffer))
}

/// Out-of-bounds (including detached) views report 0 (ES2025).
fn dv_byte_length(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (_, buffer, offset, len) = this_view(vm, a.this, "byteLength")?;
    let l = view_byte_length(vm, buffer, offset, len).unwrap_or(0);
    Ok(Value::Number(l as f64))
}

fn dv_byte_offset(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (_, buffer, offset, len) = this_view(vm, a.this, "byteOffset")?;
    match view_byte_length(vm, buffer, offset, len) {
        Some(_) => Ok(Value::Number(offset as f64)),
        None => Ok(Value::Number(0.0)),
    }
}

fn dv_get(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let kind = kind_from_tag(match vm.native_slot(a.callee, 0) {
        Value::Number(n) => n as u8,
        _ => 0,
    });
    let (_, buffer, offset, len) = this_view(vm, a.this, "get")?;
    let index = vm.to_index(a.arg(0))?;
    let little = vm.to_boolean(a.arg(1));
    let Some(view_len) = view_byte_length(vm, buffer, offset, len) else {
        return vm.type_error("DataView is out of bounds");
    };
    let size = kind.size();
    if index + size as f64 > view_len as f64 {
        return vm.range_error("Offset is outside the bounds of the DataView");
    }
    let at = offset + index as usize;
    let b = buffer_data(vm, buffer).unwrap();
    let mut bytes = b.bytes[at..at + size].to_vec();
    if !little {
        bytes.reverse();
    }
    Ok(match read_element(&bytes, kind) {
        Converted::Num(n) => Value::Number(n),
        Converted::Big(b) => vm.bigint_value(num_bigint::BigInt::from(b)),
    })
}

fn dv_set(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let kind = kind_from_tag(match vm.native_slot(a.callee, 0) {
        Value::Number(n) => n as u8,
        _ => 0,
    });
    let (_, buffer, offset, len) = this_view(vm, a.this, "set")?;
    let index = vm.to_index(a.arg(0))?;
    let value = if kind.is_bigint() {
        let b = vm.to_bigint(a.arg(1))?;
        Converted::Big(crate::bigint::to_i128_wrapping(&vm.heap.bigint(b).0))
    } else {
        Converted::Num(vm.to_number(a.arg(1))?)
    };
    let little = vm.to_boolean(a.arg(2));
    let Some(view_len) = view_byte_length(vm, buffer, offset, len) else {
        return vm.type_error("DataView is out of bounds");
    };
    let size = kind.size();
    if index + size as f64 > view_len as f64 {
        return vm.range_error("Offset is outside the bounds of the DataView");
    }
    let at = offset + index as usize;
    let mut bytes = vec![0u8; size];
    write_element(&mut bytes, kind, value);
    if !little {
        bytes.reverse();
    }
    if let Class::ArrayBuffer(b) = &mut vm.heap.obj_mut(buffer).class {
        b.bytes[at..at + size].copy_from_slice(&bytes);
    }
    Ok(Value::Undefined)
}
