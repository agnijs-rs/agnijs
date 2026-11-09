//! The Reflect object.

use crate::object::CallArgs;
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, Value};
use crate::vm::{JsResult, Vm};
use gable_engine_api::RealmId;

pub fn install(vm: &mut Vm, realm: RealmId) {
    let reflect = vm.new_object();
    vm.realm_mut().set_intrinsic(Intrinsic::Reflect, reflect);
    vm.method(reflect, "apply", 3, reflect_apply);
    vm.method(reflect, "construct", 2, reflect_construct);
    vm.method(reflect, "defineProperty", 3, reflect_define_property);
    vm.method(reflect, "deleteProperty", 2, reflect_delete_property);
    vm.method(reflect, "get", 2, reflect_get);
    vm.method(
        reflect,
        "getOwnPropertyDescriptor",
        2,
        reflect_get_own_property_descriptor,
    );
    vm.method(reflect, "getPrototypeOf", 1, reflect_get_prototype_of);
    vm.method(reflect, "has", 2, reflect_has);
    vm.method(reflect, "isExtensible", 1, reflect_is_extensible);
    vm.method(reflect, "ownKeys", 1, reflect_own_keys);
    vm.method(reflect, "preventExtensions", 1, reflect_prevent_extensions);
    vm.method(reflect, "set", 3, reflect_set);
    vm.method(reflect, "setPrototypeOf", 2, reflect_set_prototype_of);
    super::to_string_tag(vm, reflect, "Reflect");
    let global = vm.realm_of(realm).global;
    vm.define_raw_str(global, "Reflect", Value::Object(reflect), Attrs::METHOD);
}

fn target(vm: &mut Vm, v: Value, what: &str) -> JsResult<ObjId> {
    match v {
        Value::Object(o) => Ok(o),
        _ => vm.type_error(&format!("Reflect.{what} called on non-object")),
    }
}

fn reflect_apply(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let f = a.arg(0);
    if !vm.is_callable(f) {
        return vm.type_error("Reflect.apply target is not callable");
    }
    let args = vm.create_list_from_array_like(a.arg(2))?;
    vm.call_value(f, a.arg(1), &args)
}

fn reflect_construct(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let f = a.arg(0);
    if !vm.is_constructor(f) {
        return vm.type_error("Reflect.construct target is not a constructor");
    }
    let new_target = if a.len() < 3 { f } else { a.arg(2) };
    if !vm.is_constructor(new_target) {
        return vm.type_error("Reflect.construct newTarget is not a constructor");
    }
    let args = vm.create_list_from_array_like(a.arg(1))?;
    vm.construct_value(f, &args, new_target)
}

fn reflect_define_property(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = target(vm, a.arg(0), "defineProperty")?;
    let key = vm.to_property_key(a.arg(1))?;
    let desc = vm.to_property_descriptor(a.arg(2))?;
    Ok(Value::Bool(vm.define_own_property(o, key, desc)?))
}

fn reflect_delete_property(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = target(vm, a.arg(0), "deleteProperty")?;
    let key = vm.to_property_key(a.arg(1))?;
    Ok(Value::Bool(vm.delete_property(o, key)?))
}

fn reflect_get(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = target(vm, a.arg(0), "get")?;
    let key = vm.to_property_key(a.arg(1))?;
    let receiver = if a.len() < 3 { a.arg(0) } else { a.arg(2) };
    vm.get_prop(o, key, receiver)
}

fn reflect_get_own_property_descriptor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = target(vm, a.arg(0), "getOwnPropertyDescriptor")?;
    let key = vm.to_property_key(a.arg(1))?;
    match vm.get_own_property_descriptor(o, key)? {
        Some(d) => Ok(vm.from_property_descriptor(&d)),
        None => Ok(Value::Undefined),
    }
}

fn reflect_get_prototype_of(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = target(vm, a.arg(0), "getPrototypeOf")?;
    Ok(match vm.get_prototype_of(o)? {
        Some(p) => Value::Object(p),
        None => Value::Null,
    })
}

fn reflect_has(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = target(vm, a.arg(0), "has")?;
    let key = vm.to_property_key(a.arg(1))?;
    Ok(Value::Bool(vm.has_property(o, key)?))
}

fn reflect_is_extensible(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = target(vm, a.arg(0), "isExtensible")?;
    Ok(Value::Bool(vm.is_extensible(o)?))
}

fn reflect_own_keys(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = target(vm, a.arg(0), "ownKeys")?;
    let keys = vm.own_property_keys(o)?;
    let mut vals = Vec::with_capacity(keys.len());
    for k in keys {
        vals.push(vm.key_to_value(k));
    }
    Ok(Value::Object(vm.new_array(vals)))
}

fn reflect_prevent_extensions(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = target(vm, a.arg(0), "preventExtensions")?;
    Ok(Value::Bool(vm.prevent_extensions(o)?))
}

fn reflect_set(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = target(vm, a.arg(0), "set")?;
    let key = vm.to_property_key(a.arg(1))?;
    let receiver = if a.len() < 4 { a.arg(0) } else { a.arg(3) };
    Ok(Value::Bool(vm.set_prop(o, key, a.arg(2), receiver)?))
}

fn reflect_set_prototype_of(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = target(vm, a.arg(0), "setPrototypeOf")?;
    let proto = match a.arg(1) {
        Value::Object(p) => Some(p),
        Value::Null => None,
        _ => return vm.type_error("Object prototype may only be an Object or null"),
    };
    Ok(Value::Bool(vm.set_prototype_of(o, proto)?))
}
