//! The Atomics namespace (ES2025 §25.4) for a single agent. Every
//! operation is a plain read-modify-write on the buffer: the engine runs
//! one agent per host thread, so there is nobody to race with. `wait`
//! and `waitAsync` validate their arguments and report `"not-equal"` or
//! `"timed-out"` without blocking (no other agent can ever `notify`).

use agnijs_engine_api::RealmId;

use super::arraybuffer::{is_detached, is_shared};
use super::typedarray::{read_element, ta_length, write_element, Converted, ElementKind};
use super::typedarray_ctor::{ta_data, ta_kind};
use crate::object::{CallArgs, Class};
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, Value};
use crate::vm::{JsResult, Vm};

pub fn install(vm: &mut Vm, realm: RealmId) {
    let atomics = vm.new_object();
    vm.realm_mut().set_intrinsic(Intrinsic::Atomics, atomics);
    vm.method(atomics, "add", 3, atomics_add);
    vm.method(atomics, "and", 3, atomics_and);
    vm.method(atomics, "compareExchange", 4, atomics_compare_exchange);
    vm.method(atomics, "exchange", 3, atomics_exchange);
    vm.method(atomics, "isLockFree", 1, atomics_is_lock_free);
    vm.method(atomics, "load", 2, atomics_load);
    vm.method(atomics, "notify", 3, atomics_notify);
    vm.method(atomics, "or", 3, atomics_or);
    vm.method(atomics, "pause", 0, atomics_pause);
    vm.method(atomics, "store", 3, atomics_store);
    vm.method(atomics, "sub", 3, atomics_sub);
    vm.method(atomics, "wait", 4, atomics_wait);
    vm.method(atomics, "waitAsync", 4, atomics_wait_async);
    vm.method(atomics, "xor", 3, atomics_xor);
    super::to_string_tag(vm, atomics, "Atomics");
    let global = vm.realm_of(realm).global;
    vm.define_raw_str(global, "Atomics", Value::Object(atomics), Attrs::METHOD);
}

/// ValidateIntegerTypedArray: the array, its kind and its current length.
fn validate_integer_typed_array(vm: &mut Vm, v: Value, waitable: bool) -> JsResult<(ObjId, ElementKind, usize)> {
    let o = match v {
        Value::Object(o) if ta_data(vm, o).is_some() => o,
        _ => return vm.type_error("Atomics operation on a non-typed array"),
    };
    let Some(len) = ta_length(vm, o) else {
        return vm.type_error("Typed array is detached or out of bounds");
    };
    let kind = ta_kind(vm, o);
    let ok = if waitable {
        matches!(kind, ElementKind::Int32 | ElementKind::BigInt64)
    } else {
        !matches!(
            kind,
            ElementKind::Float16 | ElementKind::Float32 | ElementKind::Float64 | ElementKind::Uint8Clamped
        )
    };
    if !ok {
        return vm.type_error("Atomics operations are only allowed on integer typed arrays");
    }
    Ok((o, kind, len))
}

/// ValidateAtomicAccess: the byte index of `request` within the buffer.
fn validate_atomic_access(vm: &mut Vm, o: ObjId, len: usize, request: Value) -> JsResult<usize> {
    let index = vm.to_index(request)?;
    if index >= len as f64 {
        return vm.range_error("Atomics index out of range");
    }
    let t = ta_data(vm, o).unwrap();
    Ok(t.byte_offset + index as usize * t.kind.size())
}

fn convert_operand(vm: &mut Vm, kind: ElementKind, v: Value) -> JsResult<Converted> {
    if kind.is_bigint() {
        let b = vm.to_bigint(v)?;
        Ok(Converted::Big(crate::bigint::to_i128_wrapping(&vm.heap.bigint(b).0)))
    } else {
        let n = vm.to_integer_or_infinity(v)?;
        Ok(Converted::Num(n))
    }
}

fn revalidate(vm: &mut Vm, o: ObjId) -> JsResult<()> {
    if ta_length(vm, o).is_none() {
        return vm.type_error("Typed array is detached or out of bounds");
    }
    Ok(())
}

fn buffer_of(vm: &Vm, o: ObjId) -> ObjId {
    ta_data(vm, o).unwrap().buffer
}

fn raw_read(vm: &Vm, o: ObjId, at: usize, kind: ElementKind) -> Converted {
    let buffer = buffer_of(vm, o);
    let Class::ArrayBuffer(b) = &vm.heap.obj(buffer).class else {
        unreachable!()
    };
    read_element(&b.bytes[at..at + kind.size()], kind)
}

fn raw_write(vm: &mut Vm, o: ObjId, at: usize, kind: ElementKind, c: Converted) {
    let buffer = buffer_of(vm, o);
    if let Class::ArrayBuffer(b) = &mut vm.heap.obj_mut(buffer).class {
        write_element(&mut b.bytes[at..at + kind.size()], kind, c);
    }
}

fn to_value(vm: &mut Vm, c: Converted) -> Value {
    match c {
        Converted::Num(n) => Value::Number(n),
        Converted::Big(b) => vm.bigint_value(num_bigint::BigInt::from(b)),
    }
}

/// The integer bits of an operand as they would land in an element of `kind`.
fn as_bits(kind: ElementKind, c: Converted) -> i128 {
    let mut buf = [0u8; 8];
    write_element(&mut buf[..kind.size()], kind, c);
    match read_element(&buf[..kind.size()], kind) {
        Converted::Num(n) => n as i128,
        Converted::Big(b) => b,
    }
}

fn from_bits(kind: ElementKind, bits: i128) -> Converted {
    if kind.is_bigint() {
        Converted::Big(bits)
    } else {
        Converted::Num(bits as f64)
    }
}

/// AtomicReadModifyWrite with `op(old_bits, operand_bits) -> new_bits`.
fn rmw(vm: &mut Vm, a: &CallArgs, op: fn(i128, i128) -> i128) -> JsResult<Value> {
    let (o, kind, len) = validate_integer_typed_array(vm, a.arg(0), false)?;
    let at = validate_atomic_access(vm, o, len, a.arg(1))?;
    let operand = convert_operand(vm, kind, a.arg(2))?;
    revalidate(vm, o)?;
    let old = raw_read(vm, o, at, kind);
    let new_bits = op(as_bits(kind, old), as_bits(kind, operand));
    raw_write(vm, o, at, kind, from_bits(kind, new_bits));
    Ok(to_value(vm, old))
}

fn atomics_add(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    rmw(vm, a, |x, y| x.wrapping_add(y))
}

fn atomics_sub(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    rmw(vm, a, |x, y| x.wrapping_sub(y))
}

fn atomics_and(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    rmw(vm, a, |x, y| x & y)
}

fn atomics_or(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    rmw(vm, a, |x, y| x | y)
}

fn atomics_xor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    rmw(vm, a, |x, y| x ^ y)
}

fn atomics_exchange(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    rmw(vm, a, |_, y| y)
}

fn atomics_compare_exchange(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, kind, len) = validate_integer_typed_array(vm, a.arg(0), false)?;
    let at = validate_atomic_access(vm, o, len, a.arg(1))?;
    let expected = convert_operand(vm, kind, a.arg(2))?;
    let replacement = convert_operand(vm, kind, a.arg(3))?;
    revalidate(vm, o)?;
    let old = raw_read(vm, o, at, kind);
    if as_bits(kind, old) == as_bits(kind, expected) {
        raw_write(vm, o, at, kind, replacement);
    }
    Ok(to_value(vm, old))
}

fn atomics_load(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, kind, len) = validate_integer_typed_array(vm, a.arg(0), false)?;
    let at = validate_atomic_access(vm, o, len, a.arg(1))?;
    revalidate(vm, o)?;
    let v = raw_read(vm, o, at, kind);
    Ok(to_value(vm, v))
}

fn atomics_store(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, kind, len) = validate_integer_typed_array(vm, a.arg(0), false)?;
    let at = validate_atomic_access(vm, o, len, a.arg(1))?;
    let v = convert_operand(vm, kind, a.arg(2))?;
    revalidate(vm, o)?;
    raw_write(vm, o, at, kind, v);
    Ok(match v {
        // ToIntegerOrInfinity already normalised -0 to +0.
        Converted::Num(n) => Value::Number(if n == 0.0 { 0.0 } else { n }),
        Converted::Big(_) => to_value(vm, v),
    })
}

fn atomics_is_lock_free(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let n = vm.to_integer_or_infinity(a.arg(0))?;
    Ok(Value::Bool(matches!(n as i64, 1 | 2 | 4 | 8) && n.fract() == 0.0))
}

fn atomics_pause(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let n = a.arg(0);
    if !n.is_undefined() {
        match n {
            Value::Number(x) if x.is_finite() && x.fract() == 0.0 => {}
            _ => return vm.type_error("Atomics.pause argument must be an integral Number"),
        }
    }
    std::hint::spin_loop();
    Ok(Value::Undefined)
}

fn atomics_notify(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, _, len) = validate_integer_typed_array(vm, a.arg(0), true)?;
    let _at = validate_atomic_access(vm, o, len, a.arg(1))?;
    let count = a.arg(2);
    if !count.is_undefined() {
        let c = vm.to_integer_or_infinity(count)?;
        let _ = c.max(0.0);
    }
    // No other agent can be waiting on this buffer.
    Ok(Value::Number(0.0))
}

/// DoWait validation; returns the comparison result for the single-agent case.
fn do_wait(vm: &mut Vm, a: &CallArgs) -> JsResult<&'static str> {
    let (o, kind, len) = validate_integer_typed_array(vm, a.arg(0), true)?;
    let buffer = buffer_of(vm, o);
    if !is_shared(vm, buffer) {
        return vm.type_error("Atomics.wait requires a shared typed array");
    }
    let at = validate_atomic_access(vm, o, len, a.arg(1))?;
    let expected = convert_operand(vm, kind, a.arg(2))?;
    let timeout = a.arg(3);
    let q = vm.to_number(timeout)?;
    let _t = if q.is_nan() || q == f64::INFINITY { f64::INFINITY } else { q.max(0.0) };
    if is_detached(vm, buffer) {
        return vm.type_error("Typed array is detached");
    }
    let current = raw_read(vm, o, at, kind);
    if as_bits(kind, current) != as_bits(kind, expected) {
        return Ok("not-equal");
    }
    Ok("timed-out")
}

fn atomics_wait(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let r = do_wait(vm, a)?;
    Ok(vm.str(r))
}

fn atomics_wait_async(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let r = do_wait(vm, a)?;
    let result = vm.new_object();
    let value = vm.str(r);
    vm.define_raw_str(result, "async", Value::Bool(false), Attrs::ALL);
    vm.define_raw_str(result, "value", value, Attrs::ALL);
    Ok(Value::Object(result))
}
