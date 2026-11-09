//! Proxy exotic objects: the data model, the exotic-object hooks the object
//! operations call, and the `Proxy` constructor.

use crate::gc::Tracer;
use crate::object::{CallArgs, Class, ObjectData};
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, PropKey, PropSlot, Property, PropertyDescriptor, Value};
use crate::vm::{JsResult, Vm};

pub struct ProxyData {
    /// `None` once revoked.
    pub target: Option<ObjId>,
    pub handler: Option<ObjId>,
    pub callable: bool,
    pub constructor: bool,
}

impl ProxyData {
    pub fn trace(&self, t: &mut Tracer) {
        t.opt_obj(self.target);
        t.opt_obj(self.handler);
    }
}

fn parts(vm: &mut Vm, o: ObjId) -> JsResult<(ObjId, ObjId)> {
    match &vm.heap.obj(o).class {
        Class::Proxy(p) => match (p.target, p.handler) {
            (Some(t), Some(h)) => Ok((t, h)),
            _ => vm.type_error("Cannot perform operation on a revoked proxy"),
        },
        _ => unreachable!("not a proxy"),
    }
}

/// Fetch the trap `name` from the handler, or `None` when absent.
fn trap(vm: &mut Vm, handler: ObjId, name: &str) -> JsResult<Option<Value>> {
    let key = vm.key(name);
    vm.get_method(Value::Object(handler), key)
}

pub fn proxy_get_prototype_of(vm: &mut Vm, o: ObjId) -> JsResult<Option<ObjId>> {
    let (target, handler) = parts(vm, o)?;
    let Some(t) = trap(vm, handler, "getPrototypeOf")? else {
        return vm.get_prototype_of(target);
    };
    let r = vm.call_value(t, Value::Object(handler), &[Value::Object(target)])?;
    let proto = match r {
        Value::Object(p) => Some(p),
        Value::Null => None,
        _ => {
            return vm
                .type_error("'getPrototypeOf' on proxy: trap returned neither object nor null")
        }
    };
    if vm.is_extensible(target)? {
        return Ok(proto);
    }
    let tp = vm.get_prototype_of(target)?;
    if tp != proto {
        return vm.type_error("'getPrototypeOf' on proxy: proxy target is non-extensible but the trap did not return its actual prototype");
    }
    Ok(proto)
}

pub fn proxy_set_prototype_of(vm: &mut Vm, o: ObjId, proto: Option<ObjId>) -> JsResult<bool> {
    let (target, handler) = parts(vm, o)?;
    let Some(t) = trap(vm, handler, "setPrototypeOf")? else {
        return vm.set_prototype_of(target, proto);
    };
    let pv = proto.map(Value::Object).unwrap_or(Value::Null);
    let r = vm.call_value(t, Value::Object(handler), &[Value::Object(target), pv])?;
    if !vm.to_boolean(r) {
        return Ok(false);
    }
    if vm.is_extensible(target)? {
        return Ok(true);
    }
    let tp = vm.get_prototype_of(target)?;
    if tp != proto {
        return vm.type_error("'setPrototypeOf' on proxy: trap returned truish for setting a new prototype on the non-extensible proxy target");
    }
    Ok(true)
}

pub fn proxy_is_extensible(vm: &mut Vm, o: ObjId) -> JsResult<bool> {
    let (target, handler) = parts(vm, o)?;
    let Some(t) = trap(vm, handler, "isExtensible")? else {
        return vm.is_extensible(target);
    };
    let r = vm.call_value(t, Value::Object(handler), &[Value::Object(target)])?;
    let b = vm.to_boolean(r);
    if b != vm.is_extensible(target)? {
        return vm.type_error(
            "'isExtensible' on proxy: trap result does not reflect extensibility of proxy target",
        );
    }
    Ok(b)
}

pub fn proxy_prevent_extensions(vm: &mut Vm, o: ObjId) -> JsResult<bool> {
    let (target, handler) = parts(vm, o)?;
    let Some(t) = trap(vm, handler, "preventExtensions")? else {
        return vm.prevent_extensions(target);
    };
    let r = vm.call_value(t, Value::Object(handler), &[Value::Object(target)])?;
    let b = vm.to_boolean(r);
    if b && vm.is_extensible(target)? {
        return vm.type_error(
            "'preventExtensions' on proxy: trap returned truish but the proxy target is extensible",
        );
    }
    Ok(b)
}

/// IsCompatiblePropertyDescriptor.
fn is_compatible(
    vm: &mut Vm,
    extensible: bool,
    desc: &PropertyDescriptor,
    current: Option<Property>,
) -> bool {
    vm.validate_and_apply(None, PropKey::Index(0), extensible, desc, current)
}

pub fn proxy_get_own_property(vm: &mut Vm, o: ObjId, key: PropKey) -> JsResult<Option<Property>> {
    let (target, handler) = parts(vm, o)?;
    let Some(t) = trap(vm, handler, "getOwnPropertyDescriptor")? else {
        return vm.own_property(target, key);
    };
    let kv = vm.key_to_value(key);
    let r = vm.call_value(t, Value::Object(handler), &[Value::Object(target), kv])?;
    if !r.is_object() && !r.is_undefined() {
        return vm.type_error(
            "'getOwnPropertyDescriptor' on proxy: trap returned neither object nor undefined",
        );
    }
    let target_desc = vm.own_property(target, key)?;
    if r.is_undefined() {
        let Some(td) = target_desc else {
            return Ok(None);
        };
        if !td.attrs.configurable() {
            return vm.type_error("'getOwnPropertyDescriptor' on proxy: trap returned undefined for a non-configurable property");
        }
        if !vm.is_extensible(target)? {
            return vm.type_error("'getOwnPropertyDescriptor' on proxy: trap returned undefined for a property of a non-extensible target");
        }
        return Ok(None);
    }
    let extensible = vm.is_extensible(target)?;
    let mut desc = vm.to_property_descriptor(r)?;
    complete_descriptor(&mut desc);
    if !is_compatible(vm, extensible, &desc, target_desc) {
        return vm.type_error("'getOwnPropertyDescriptor' on proxy: trap returned descriptor incompatible with the target");
    }
    if desc.configurable == Some(false) {
        match target_desc {
            None => return vm.type_error("'getOwnPropertyDescriptor' on proxy: trap reported non-configurability for a property that does not exist on the target"),
            Some(td) => {
                if td.attrs.configurable() {
                    return vm.type_error("'getOwnPropertyDescriptor' on proxy: trap reported non-configurability for a configurable property");
                }
                if desc.writable == Some(false) && td.attrs.writable() {
                    return vm.type_error("'getOwnPropertyDescriptor' on proxy: trap reported non-configurable and non-writable for a writable property");
                }
            }
        }
    }
    Ok(Some(descriptor_to_property(&desc)))
}

/// CompletePropertyDescriptor.
pub fn complete_descriptor(d: &mut PropertyDescriptor) {
    if d.is_generic() || d.is_data() {
        d.value.get_or_insert(Value::Undefined);
        d.writable.get_or_insert(false);
    } else {
        d.get.get_or_insert(Value::Undefined);
        d.set.get_or_insert(Value::Undefined);
    }
    d.enumerable.get_or_insert(false);
    d.configurable.get_or_insert(false);
}

/// A complete descriptor as a property record.
pub fn descriptor_to_property(d: &PropertyDescriptor) -> Property {
    let attrs = Attrs::from_flags(
        d.writable == Some(true),
        d.enumerable == Some(true),
        d.configurable == Some(true),
    );
    if d.is_accessor() {
        Property::accessor(
            d.get.unwrap_or(Value::Undefined),
            d.set.unwrap_or(Value::Undefined),
            attrs,
        )
    } else {
        Property::data(d.value.unwrap_or(Value::Undefined), attrs)
    }
}

pub fn proxy_define_own_property(
    vm: &mut Vm,
    o: ObjId,
    key: PropKey,
    desc: PropertyDescriptor,
) -> JsResult<bool> {
    let (target, handler) = parts(vm, o)?;
    let Some(t) = trap(vm, handler, "defineProperty")? else {
        return vm.define_own_property(target, key, desc);
    };
    let kv = vm.key_to_value(key);
    let dv = vm.from_property_descriptor(&desc);
    let r = vm.call_value(t, Value::Object(handler), &[Value::Object(target), kv, dv])?;
    if !vm.to_boolean(r) {
        return Ok(false);
    }
    let target_desc = vm.own_property(target, key)?;
    let extensible = vm.is_extensible(target)?;
    let setting_non_config = desc.configurable == Some(false);
    match target_desc {
        None => {
            if !extensible {
                return vm.type_error("'defineProperty' on proxy: trap returned truish for adding a property to the non-extensible proxy target");
            }
            if setting_non_config {
                return vm.type_error("'defineProperty' on proxy: trap returned truish for defining a non-configurable property which does not exist on the target");
            }
        }
        Some(td) => {
            if !is_compatible(vm, extensible, &desc, Some(td)) {
                return vm.type_error("'defineProperty' on proxy: trap returned truish for adding a property that is incompatible with the existing property in the proxy target");
            }
            if setting_non_config && td.attrs.configurable() {
                return vm.type_error("'defineProperty' on proxy: trap returned truish for defining a non-configurable property which is configurable on the target");
            }
            if !td.is_accessor()
                && !td.attrs.configurable()
                && td.attrs.writable()
                && desc.writable == Some(false)
            {
                return vm.type_error("'defineProperty' on proxy: trap returned truish for defining a non-writable property which is writable on the target");
            }
        }
    }
    Ok(true)
}

pub fn proxy_has(vm: &mut Vm, o: ObjId, key: PropKey) -> JsResult<bool> {
    let (target, handler) = parts(vm, o)?;
    let Some(t) = trap(vm, handler, "has")? else {
        return vm.has_property(target, key);
    };
    let kv = vm.key_to_value(key);
    let r = vm.call_value(t, Value::Object(handler), &[Value::Object(target), kv])?;
    let b = vm.to_boolean(r);
    if !b {
        if let Some(td) = vm.own_property(target, key)? {
            if !td.attrs.configurable() {
                return vm.type_error(
                    "'has' on proxy: trap returned falsish for a non-configurable property",
                );
            }
            if !vm.is_extensible(target)? {
                return vm.type_error("'has' on proxy: trap returned falsish for a property of a non-extensible target");
            }
        }
    }
    Ok(b)
}

pub fn proxy_get(vm: &mut Vm, o: ObjId, key: PropKey, receiver: Value) -> JsResult<Value> {
    let (target, handler) = parts(vm, o)?;
    let Some(t) = trap(vm, handler, "get")? else {
        return vm.get_prop(target, key, receiver);
    };
    let kv = vm.key_to_value(key);
    let r = vm.call_value(
        t,
        Value::Object(handler),
        &[Value::Object(target), kv, receiver],
    )?;
    if let Some(td) = vm.own_property(target, key)? {
        if !td.attrs.configurable() {
            match td.slot {
                PropSlot::Data(v) if !td.attrs.writable() => {
                    if !vm.same_value(r, v) {
                        return vm.type_error("'get' on proxy: trap result differs from the non-writable, non-configurable target property");
                    }
                }
                PropSlot::Accessor { get, .. } if get.is_undefined() => {
                    if !r.is_undefined() {
                        return vm.type_error("'get' on proxy: trap returned a value for a non-configurable accessor without a getter");
                    }
                }
                _ => {}
            }
        }
    }
    Ok(r)
}

pub fn proxy_set(vm: &mut Vm, o: ObjId, key: PropKey, v: Value, receiver: Value) -> JsResult<bool> {
    let (target, handler) = parts(vm, o)?;
    let Some(t) = trap(vm, handler, "set")? else {
        return vm.set_prop(target, key, v, receiver);
    };
    let kv = vm.key_to_value(key);
    let r = vm.call_value(
        t,
        Value::Object(handler),
        &[Value::Object(target), kv, v, receiver],
    )?;
    if !vm.to_boolean(r) {
        return Ok(false);
    }
    if let Some(td) = vm.own_property(target, key)? {
        if !td.attrs.configurable() {
            match td.slot {
                PropSlot::Data(tv) if !td.attrs.writable() => {
                    if !vm.same_value(v, tv) {
                        return vm.type_error("'set' on proxy: trap returned truish for a non-writable, non-configurable property with a different value");
                    }
                }
                PropSlot::Accessor { set, .. } if set.is_undefined() => {
                    return vm.type_error("'set' on proxy: trap returned truish for a non-configurable accessor without a setter");
                }
                _ => {}
            }
        }
    }
    Ok(true)
}

pub fn proxy_delete(vm: &mut Vm, o: ObjId, key: PropKey) -> JsResult<bool> {
    let (target, handler) = parts(vm, o)?;
    let Some(t) = trap(vm, handler, "deleteProperty")? else {
        return vm.delete_property(target, key);
    };
    let kv = vm.key_to_value(key);
    let r = vm.call_value(t, Value::Object(handler), &[Value::Object(target), kv])?;
    if !vm.to_boolean(r) {
        return Ok(false);
    }
    let Some(td) = vm.own_property(target, key)? else {
        return Ok(true);
    };
    if !td.attrs.configurable() {
        return vm.type_error(
            "'deleteProperty' on proxy: trap returned truish for a non-configurable property",
        );
    }
    if !vm.is_extensible(target)? {
        return vm.type_error("'deleteProperty' on proxy: trap returned truish for a property of a non-extensible target");
    }
    Ok(true)
}

pub fn proxy_own_keys(vm: &mut Vm, o: ObjId) -> JsResult<Vec<PropKey>> {
    let (target, handler) = parts(vm, o)?;
    let Some(t) = trap(vm, handler, "ownKeys")? else {
        return vm.own_property_keys(target);
    };
    let r = vm.call_value(t, Value::Object(handler), &[Value::Object(target)])?;
    if !r.is_object() {
        return vm.type_error("'ownKeys' on proxy: trap result is not an object");
    }
    let list = vm.create_list_from_array_like(r)?;
    let mut keys: Vec<PropKey> = Vec::with_capacity(list.len());
    for v in list {
        let k = match v {
            Value::String(_) | Value::Symbol(_) => vm.to_property_key(v)?,
            _ => {
                return vm.type_error("'ownKeys' on proxy: trap result contains a non-property-key")
            }
        };
        if keys.contains(&k) {
            return vm.type_error("'ownKeys' on proxy: trap returned duplicate entries");
        }
        keys.push(k);
    }
    let extensible = vm.is_extensible(target)?;
    let target_keys = vm.own_property_keys(target)?;
    let mut configurable = Vec::new();
    let mut nonconfigurable = Vec::new();
    for k in target_keys {
        match vm.own_property(target, k)? {
            Some(p) if !p.attrs.configurable() => nonconfigurable.push(k),
            _ => configurable.push(k),
        }
    }
    if extensible && nonconfigurable.is_empty() {
        return Ok(keys);
    }
    let mut unchecked: Vec<Option<PropKey>> = keys.iter().copied().map(Some).collect();
    for k in nonconfigurable {
        let Some(slot) = unchecked.iter_mut().find(|s| **s == Some(k)) else {
            return vm.type_error(
                "'ownKeys' on proxy: trap result did not include a non-configurable key",
            );
        };
        *slot = None;
    }
    if extensible {
        return Ok(keys);
    }
    for k in configurable {
        let Some(slot) = unchecked.iter_mut().find(|s| **s == Some(k)) else {
            return vm.type_error("'ownKeys' on proxy: trap result did not include a key of the non-extensible target");
        };
        *slot = None;
    }
    if unchecked.iter().any(|s| s.is_some()) {
        return vm.type_error(
            "'ownKeys' on proxy: trap returned extra keys but proxy target is non-extensible",
        );
    }
    Ok(keys)
}

pub fn proxy_call(vm: &mut Vm, o: ObjId, this: Value, args: &[Value]) -> JsResult<Value> {
    let (target, handler) = parts(vm, o)?;
    let Some(t) = trap(vm, handler, "apply")? else {
        return vm.call_value(Value::Object(target), this, args);
    };
    let arr = vm.new_array(args.to_vec());
    vm.call_value(
        t,
        Value::Object(handler),
        &[Value::Object(target), this, Value::Object(arr)],
    )
}

pub fn proxy_construct(
    vm: &mut Vm,
    o: ObjId,
    args: &[Value],
    new_target: Value,
) -> JsResult<Value> {
    let (target, handler) = parts(vm, o)?;
    let Some(t) = trap(vm, handler, "construct")? else {
        return vm.construct_value(Value::Object(target), args, new_target);
    };
    let arr = vm.new_array(args.to_vec());
    let r = vm.call_value(
        t,
        Value::Object(handler),
        &[Value::Object(target), Value::Object(arr), new_target],
    )?;
    if !r.is_object() {
        return vm.type_error("proxy [[Construct]] must return an object");
    }
    Ok(r)
}

// ---- constructor ---------------------------------------------------------------------

pub fn install(vm: &mut Vm, realm: agnijs_engine_api::RealmId) {
    let ctor = vm.new_native_ctor("Proxy", 2, proxy_constructor);
    vm.realm_mut().set_intrinsic(Intrinsic::Proxy, ctor);
    vm.method(ctor, "revocable", 2, proxy_revocable);
    let global = vm.realm_of(realm).global;
    vm.define_raw_str(global, "Proxy", Value::Object(ctor), Attrs::METHOD);
}

pub fn proxy_create(vm: &mut Vm, target: Value, handler: Value) -> JsResult<ObjId> {
    let (Value::Object(t), Value::Object(h)) = (target, handler) else {
        return vm.type_error("Cannot create proxy with a non-object as target or handler");
    };
    let data = ProxyData {
        target: Some(t),
        handler: Some(h),
        callable: vm.is_callable(target),
        constructor: vm.is_constructor(target),
    };
    Ok(vm.alloc(ObjectData::new(None, Class::Proxy(Box::new(data)))))
}

fn proxy_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if !a.is_construct() {
        return vm.type_error("Constructor Proxy requires 'new'");
    }
    Ok(Value::Object(proxy_create(vm, a.arg(0), a.arg(1))?))
}

fn proxy_revocable(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let p = proxy_create(vm, a.arg(0), a.arg(1))?;
    let revoke = vm.new_native_closure("", 0, proxy_revoke, vec![Value::Object(p)]);
    let o = vm.new_object();
    vm.define_raw_str(o, "proxy", Value::Object(p), Attrs::ALL);
    vm.define_raw_str(o, "revoke", Value::Object(revoke), Attrs::ALL);
    Ok(Value::Object(o))
}

fn proxy_revoke(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let p = vm.native_slot(a.callee, 0);
    if let Value::Object(po) = p {
        if let Class::Proxy(d) = &mut vm.heap.obj_mut(po).class {
            d.target = None;
            d.handler = None;
        }
        vm.heap.obj_mut(a.callee).native_mut().unwrap().slots[0] = Value::Null;
    }
    Ok(Value::Undefined)
}
