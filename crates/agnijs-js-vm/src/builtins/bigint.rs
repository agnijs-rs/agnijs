//! BigInt constructor and BigInt.prototype.

use crate::bigint;
use crate::object::{CallArgs, Class, ObjectData};
use crate::realm::Intrinsic;
use crate::value::{BigId, Value};
use crate::vm::{JsResult, Vm};
use agnijs_engine_api::RealmId;
use num_bigint::BigInt;

pub fn install(vm: &mut Vm, realm: RealmId) {
    let object_proto = vm.intrinsic(Intrinsic::ObjectPrototype);
    let proto = vm.new_object_with_proto(Some(object_proto));
    let ctor = super::install_ctor(
        vm,
        realm,
        "BigInt",
        1,
        bigint_constructor,
        proto,
        Intrinsic::BigInt,
        Intrinsic::BigIntPrototype,
    );
    vm.method(ctor, "asIntN", 2, bigint_as_int_n);
    vm.method(ctor, "asUintN", 2, bigint_as_uint_n);
    vm.method(proto, "toLocaleString", 0, proto_to_locale_string);
    vm.method(proto, "toString", 0, proto_to_string);
    vm.method(proto, "valueOf", 0, proto_value_of);
    super::to_string_tag(vm, proto, "BigInt");
}

fn bigint_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if a.is_construct() {
        return vm.type_error("BigInt is not a constructor");
    }
    let prim = vm.to_primitive(a.arg(0), crate::convert::Hint::Number)?;
    if let Value::Number(n) = prim {
        return number_to_bigint(vm, n);
    }
    let b = vm.to_bigint(prim)?;
    Ok(Value::BigInt(b))
}

/// NumberToBigInt(number).
pub fn number_to_bigint(vm: &mut Vm, n: f64) -> JsResult<Value> {
    match bigint::from_f64(n) {
        Some(b) => Ok(vm.bigint_value(b)),
        None => {
            let s = agnijs_js_parser::number::to_js_string(n);
            vm.range_error(&format!(
                "The number {s} cannot be converted to a BigInt because it is not an integer"
            ))
        }
    }
}

fn bigint_as_int_n(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let bits = vm.to_index(a.arg(0))?;
    let b = vm.to_bigint(a.arg(1))?;
    let r = bigint::as_int_n(bits as u64, &vm.heap.bigint(b).0);
    Ok(vm.bigint_value(r))
}

fn bigint_as_uint_n(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let bits = vm.to_index(a.arg(0))?;
    let b = vm.to_bigint(a.arg(1))?;
    let r = bigint::as_uint_n(bits as u64, &vm.heap.bigint(b).0);
    Ok(vm.bigint_value(r))
}

fn this_bigint_value(vm: &mut Vm, v: Value) -> JsResult<BigId> {
    match v {
        Value::BigInt(b) => Ok(b),
        Value::Object(o) => match vm.heap.obj(o).class {
            Class::BigInt(b) => Ok(b),
            _ => vm.type_error("BigInt.prototype method called on incompatible receiver"),
        },
        _ => vm.type_error("BigInt.prototype method called on incompatible receiver"),
    }
}

fn proto_value_of(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    Ok(Value::BigInt(this_bigint_value(vm, a.this)?))
}

fn proto_to_locale_string(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let b = this_bigint_value(vm, a.this)?;
    #[cfg(feature = "intl")]
    return super::intl::numberformat::to_locale_string(vm, Value::BigInt(b), a.arg(0), a.arg(1));
    // Without ECMA-402, ToString (ECMA-262 leaves the form open).
    #[cfg(not(feature = "intl"))]
    Ok(Value::String(vm.to_string(Value::BigInt(b))?))
}

fn proto_to_string(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let b = this_bigint_value(vm, a.this)?;
    let radix = if a.arg(0).is_undefined() {
        10.0
    } else {
        vm.to_integer_or_infinity(a.arg(0))?
    };
    if !(2.0..=36.0).contains(&radix) {
        return vm.range_error("toString() radix must be between 2 and 36");
    }
    let s = bigint::to_string_radix(&vm.heap.bigint(b).0, radix as u32);
    Ok(vm.str(&s))
}

/// Wrap a BigInt in a BigInt object (ToObject).
pub fn bigint_object(vm: &mut Vm, b: BigId, proto: crate::value::ObjId) -> crate::value::ObjId {
    vm.alloc(ObjectData::new(Some(proto), Class::BigInt(b)))
}

#[allow(dead_code)]
fn _ty(_b: &BigInt) {}
