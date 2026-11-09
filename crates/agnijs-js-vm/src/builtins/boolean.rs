//! Boolean constructor and Boolean.prototype.

use crate::object::{CallArgs, Class, ObjectData};
use crate::realm::Intrinsic;
use crate::value::Value;
use crate::vm::{JsResult, Vm};
use agnijs_engine_api::RealmId;

pub fn install(vm: &mut Vm, realm: RealmId) {
    let object_proto = vm.intrinsic(Intrinsic::ObjectPrototype);
    let proto = vm.alloc(ObjectData::new(Some(object_proto), Class::Boolean(false)));
    super::install_ctor(
        vm,
        realm,
        "Boolean",
        1,
        boolean_constructor,
        proto,
        Intrinsic::Boolean,
        Intrinsic::BooleanPrototype,
    );
    vm.method(proto, "toString", 0, proto_to_string);
    vm.method(proto, "valueOf", 0, proto_value_of);
}

fn boolean_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let b = vm.to_boolean(a.arg(0));
    if !a.is_construct() {
        return Ok(Value::Bool(b));
    }
    let o = vm.ordinary_create_from_constructor(
        a.new_target,
        Intrinsic::BooleanPrototype,
        Class::Boolean(b),
    )?;
    Ok(Value::Object(o))
}

fn this_boolean_value(vm: &mut Vm, v: Value) -> JsResult<bool> {
    match v {
        Value::Bool(b) => Ok(b),
        Value::Object(o) => match vm.heap.obj(o).class {
            Class::Boolean(b) => Ok(b),
            _ => vm.type_error("Boolean.prototype method called on incompatible receiver"),
        },
        _ => vm.type_error("Boolean.prototype method called on incompatible receiver"),
    }
}

fn proto_to_string(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let b = this_boolean_value(vm, a.this)?;
    Ok(Value::String(if b {
        vm.names.true_
    } else {
        vm.names.false_
    }))
}

fn proto_value_of(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    Ok(Value::Bool(this_boolean_value(vm, a.this)?))
}
