//! Object constructor and Object.prototype.

use crate::object::{CallArgs, Class};
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, PropKey, PropSlot, PropertyDescriptor, Value};
use crate::vm::{JsResult, Vm};
use agnijs_engine_api::RealmId;

pub fn install(vm: &mut Vm, realm: RealmId) {
    let proto = vm.intrinsic(Intrinsic::ObjectPrototype);
    let ctor = super::install_ctor(
        vm,
        realm,
        "Object",
        1,
        object_constructor,
        proto,
        Intrinsic::Object,
        Intrinsic::ObjectPrototype,
    );

    vm.method(ctor, "assign", 2, object_assign);
    vm.method(ctor, "create", 2, object_create);
    vm.method(ctor, "defineProperties", 2, object_define_properties);
    vm.method(ctor, "defineProperty", 3, object_define_property);
    vm.method(ctor, "entries", 1, object_entries);
    vm.method(ctor, "freeze", 1, object_freeze);
    vm.method(ctor, "fromEntries", 1, object_from_entries);
    vm.method(
        ctor,
        "getOwnPropertyDescriptor",
        2,
        object_get_own_property_descriptor,
    );
    vm.method(
        ctor,
        "getOwnPropertyDescriptors",
        1,
        object_get_own_property_descriptors,
    );
    vm.method(
        ctor,
        "getOwnPropertyNames",
        1,
        object_get_own_property_names,
    );
    vm.method(
        ctor,
        "getOwnPropertySymbols",
        1,
        object_get_own_property_symbols,
    );
    vm.method(ctor, "getPrototypeOf", 1, object_get_prototype_of);
    vm.method(ctor, "groupBy", 2, object_group_by);
    vm.method(ctor, "hasOwn", 2, object_has_own);
    vm.method(ctor, "is", 2, object_is);
    vm.method(ctor, "isExtensible", 1, object_is_extensible);
    vm.method(ctor, "isFrozen", 1, object_is_frozen);
    vm.method(ctor, "isSealed", 1, object_is_sealed);
    vm.method(ctor, "keys", 1, object_keys);
    vm.method(ctor, "preventExtensions", 1, object_prevent_extensions);
    vm.method(ctor, "seal", 1, object_seal);
    vm.method(ctor, "setPrototypeOf", 2, object_set_prototype_of);
    vm.method(ctor, "values", 1, object_values);

    vm.method(proto, "hasOwnProperty", 1, proto_has_own_property);
    vm.method(proto, "isPrototypeOf", 1, proto_is_prototype_of);
    vm.method(
        proto,
        "propertyIsEnumerable",
        1,
        proto_property_is_enumerable,
    );
    vm.method(proto, "toLocaleString", 0, proto_to_locale_string);
    vm.method(proto, "toString", 0, proto_to_string);
    vm.method(proto, "valueOf", 0, proto_value_of);
    vm.method(proto, "__defineGetter__", 2, proto_define_getter);
    vm.method(proto, "__defineSetter__", 2, proto_define_setter);
    vm.method(proto, "__lookupGetter__", 1, proto_lookup_getter);
    vm.method(proto, "__lookupSetter__", 1, proto_lookup_setter);
    vm.accessor(proto, "__proto__", proto_get_proto, Some(proto_set_proto));
}

fn object_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if a.is_construct() {
        let active = Value::Object(a.callee);
        if a.new_target != active {
            let o = vm.ordinary_create_from_constructor(
                a.new_target,
                Intrinsic::ObjectPrototype,
                Class::Ordinary,
            )?;
            return Ok(Value::Object(o));
        }
    }
    let v = a.arg(0);
    if v.is_nullish() {
        return Ok(Value::Object(vm.new_object()));
    }
    Ok(Value::Object(vm.to_object(v)?))
}

fn object_assign(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let to = vm.to_object(a.arg(0))?;
    for src in a.args.iter().skip(1) {
        if src.is_nullish() {
            continue;
        }
        let from = vm.to_object(*src)?;
        let keys = vm.own_property_keys(from)?;
        for k in keys {
            vm.poll_interrupt()?;
            if let Some(p) = vm.own_property(from, k)? {
                if p.attrs.enumerable() {
                    let v = vm.get(from, k)?;
                    vm.set(to, k, v, true)?;
                }
            }
        }
    }
    Ok(Value::Object(to))
}

fn object_create(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let proto = match a.arg(0) {
        Value::Object(o) => Some(o),
        Value::Null => None,
        _ => return vm.type_error("Object prototype may only be an Object or null"),
    };
    let o = vm.new_object_with_proto(proto);
    if !a.arg(1).is_undefined() {
        define_properties(vm, o, a.arg(1))?;
    }
    Ok(Value::Object(o))
}

/// ObjectDefineProperties(O, Properties).
pub fn define_properties(vm: &mut Vm, o: ObjId, props: Value) -> JsResult<()> {
    let props = vm.to_object(props)?;
    let keys = vm.own_property_keys(props)?;
    let mut descriptors = Vec::new();
    for k in keys {
        vm.poll_interrupt()?;
        if let Some(p) = vm.own_property(props, k)? {
            if p.attrs.enumerable() {
                let dv = vm.get(props, k)?;
                let desc = vm.to_property_descriptor(dv)?;
                descriptors.push((k, desc));
            }
        }
    }
    for (k, desc) in descriptors {
        vm.poll_interrupt()?;
        vm.define_property_or_throw(o, k, desc)?;
    }
    Ok(())
}

fn object_define_properties(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Value::Object(o) = a.arg(0) else {
        return vm.type_error("Object.defineProperties called on non-object");
    };
    define_properties(vm, o, a.arg(1))?;
    Ok(Value::Object(o))
}

fn object_define_property(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Value::Object(o) = a.arg(0) else {
        return vm.type_error("Object.defineProperty called on non-object");
    };
    let key = vm.to_property_key(a.arg(1))?;
    let desc = vm.to_property_descriptor(a.arg(2))?;
    vm.define_property_or_throw(o, key, desc)?;
    Ok(Value::Object(o))
}

fn object_entries(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = vm.to_object(a.arg(0))?;
    let list = vm.enumerable_own_properties(o, 2)?;
    Ok(Value::Object(vm.new_array(list)))
}

fn object_keys(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = vm.to_object(a.arg(0))?;
    let list = vm.enumerable_own_properties(o, 0)?;
    Ok(Value::Object(vm.new_array(list)))
}

fn object_values(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = vm.to_object(a.arg(0))?;
    let list = vm.enumerable_own_properties(o, 1)?;
    Ok(Value::Object(vm.new_array(list)))
}

fn object_freeze(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Value::Object(o) = a.arg(0) else {
        return Ok(a.arg(0));
    };
    if !vm.set_integrity_level(o, true)? {
        return vm.type_error("Cannot freeze object");
    }
    Ok(a.arg(0))
}

fn object_seal(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Value::Object(o) = a.arg(0) else {
        return Ok(a.arg(0));
    };
    if !vm.set_integrity_level(o, false)? {
        return vm.type_error("Cannot seal object");
    }
    Ok(a.arg(0))
}

fn object_prevent_extensions(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Value::Object(o) = a.arg(0) else {
        return Ok(a.arg(0));
    };
    if !vm.prevent_extensions(o)? {
        return vm.type_error("Cannot prevent extensions");
    }
    Ok(a.arg(0))
}

fn object_is_frozen(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Value::Object(o) = a.arg(0) else {
        return Ok(Value::Bool(true));
    };
    Ok(Value::Bool(vm.test_integrity_level(o, true)?))
}

fn object_is_sealed(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Value::Object(o) = a.arg(0) else {
        return Ok(Value::Bool(true));
    };
    Ok(Value::Bool(vm.test_integrity_level(o, false)?))
}

fn object_is_extensible(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Value::Object(o) = a.arg(0) else {
        return Ok(Value::Bool(false));
    };
    Ok(Value::Bool(vm.is_extensible(o)?))
}

fn object_from_entries(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let iterable = a.arg(0);
    if iterable.is_nullish() {
        return vm.type_error("Object.fromEntries requires an iterable");
    }
    let obj = vm.new_object();
    let (iter, next) = vm.get_iterator(iterable, false)?;
    loop {
        vm.poll_interrupt()?;
        let Some(entry) = vm.iterator_step_value(iter, next)? else {
            break;
        };
        let r: JsResult<()> = (|| {
            let Value::Object(e) = entry else {
                return vm.type_error("Iterator value is not an entry object");
            };
            let k = vm.get(e, PropKey::Index(0))?;
            let v = vm.get(e, PropKey::Index(1))?;
            let key = vm.to_property_key(k)?;
            vm.create_data_property_or_throw(obj, key, v)
        })();
        if let Err(f) = r {
            vm.iterator_close_swallow(iter);
            return Err(f);
        }
    }
    Ok(Value::Object(obj))
}

fn object_get_own_property_descriptor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = vm.to_object(a.arg(0))?;
    let key = vm.to_property_key(a.arg(1))?;
    match vm.get_own_property_descriptor(o, key)? {
        Some(d) => Ok(vm.from_property_descriptor(&d)),
        None => Ok(Value::Undefined),
    }
}

fn object_get_own_property_descriptors(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = vm.to_object(a.arg(0))?;
    let keys = vm.own_property_keys(o)?;
    let result = vm.new_object();
    for k in keys {
        vm.poll_interrupt()?;
        if let Some(d) = vm.get_own_property_descriptor(o, k)? {
            let dv = vm.from_property_descriptor(&d);
            vm.create_data_property_or_throw(result, k, dv)?;
        }
    }
    Ok(Value::Object(result))
}

fn own_keys_of_kind(vm: &mut Vm, v: Value, symbols: bool) -> JsResult<Value> {
    let o = vm.to_object(v)?;
    let keys = vm.own_property_keys(o)?;
    let mut out = Vec::new();
    for k in keys {
        vm.poll_interrupt()?;
        let is_sym = matches!(k, PropKey::Sym(_));
        if is_sym == symbols {
            out.push(vm.key_to_value(k));
        }
    }
    Ok(Value::Object(vm.new_array(out)))
}

fn object_get_own_property_names(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    own_keys_of_kind(vm, a.arg(0), false)
}

fn object_get_own_property_symbols(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    own_keys_of_kind(vm, a.arg(0), true)
}

fn object_get_prototype_of(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = vm.to_object(a.arg(0))?;
    Ok(match vm.get_prototype_of(o)? {
        Some(p) => Value::Object(p),
        None => Value::Null,
    })
}

fn object_set_prototype_of(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = a.arg(0);
    if o.is_nullish() {
        return vm.type_error("Object.setPrototypeOf called on null or undefined");
    }
    let proto = match a.arg(1) {
        Value::Object(p) => Some(p),
        Value::Null => None,
        _ => return vm.type_error("Object prototype may only be an Object or null"),
    };
    let Value::Object(obj) = o else { return Ok(o) };
    if !vm.set_prototype_of(obj, proto)? {
        return vm.type_error("Cannot set prototype");
    }
    Ok(o)
}

/// GroupBy(items, callback, key-coercion): returns (key, group) pairs in order.
pub fn group_by(
    vm: &mut Vm,
    items: Value,
    callback: Value,
    property_keys: bool,
) -> JsResult<Vec<(Value, Vec<Value>)>> {
    if items.is_nullish() {
        return vm.type_error("groupBy called on null or undefined");
    }
    if !vm.is_callable(callback) {
        return vm.type_error("groupBy callback is not a function");
    }
    let mut groups: Vec<(Value, Vec<Value>)> = Vec::new();
    let (iter, next) = vm.get_iterator(items, false)?;
    let mut k = 0f64;
    loop {
        vm.poll_interrupt()?;
        if k >= 9007199254740991.0 {
            let e = vm.make_error(
                crate::vm::ErrorType::TypeError,
                "groupBy: too many elements",
            );
            vm.iterator_close_swallow(iter);
            return Err(crate::vm::Fault::Throw(e));
        }
        let Some(v) = vm.iterator_step_value(iter, next)? else {
            break;
        };
        let key = match vm.call_value(callback, Value::Undefined, &[v, Value::Number(k)]) {
            Ok(key) => key,
            Err(f) => {
                vm.iterator_close_swallow(iter);
                return Err(f);
            }
        };
        let key = if property_keys {
            match vm.to_property_key(key) {
                Ok(pk) => vm.key_to_value(pk),
                Err(f) => {
                    vm.iterator_close_swallow(iter);
                    return Err(f);
                }
            }
        } else if key == Value::Number(-0.0) {
            Value::Number(0.0)
        } else {
            key
        };
        let found = groups.iter_mut().find(|(gk, _)| vm.same_value(*gk, key));
        match found {
            Some((_, list)) => list.push(v),
            None => groups.push((key, vec![v])),
        }
        k += 1.0;
    }
    Ok(groups)
}

fn object_group_by(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let groups = group_by(vm, a.arg(0), a.arg(1), true)?;
    let obj = vm.new_object_with_proto(None);
    for (k, list) in groups {
        vm.poll_interrupt()?;
        let key = vm.to_property_key(k)?;
        let arr = vm.new_array(list);
        vm.create_data_property_or_throw(obj, key, Value::Object(arr))?;
    }
    Ok(Value::Object(obj))
}

fn object_has_own(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = vm.to_object(a.arg(0))?;
    let key = vm.to_property_key(a.arg(1))?;
    Ok(Value::Bool(vm.has_own_property(o, key)?))
}

fn object_is(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    Ok(Value::Bool(vm.same_value(a.arg(0), a.arg(1))))
}

// ---- prototype ----------------------------------------------------------------------------

fn proto_has_own_property(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let key = vm.to_property_key(a.arg(0))?;
    let o = vm.to_object(a.this)?;
    Ok(Value::Bool(vm.has_own_property(o, key)?))
}

fn proto_is_prototype_of(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Value::Object(mut v) = a.arg(0) else {
        return Ok(Value::Bool(false));
    };
    let o = vm.to_object(a.this)?;
    loop {
        match vm.get_prototype_of(v)? {
            None => return Ok(Value::Bool(false)),
            Some(p) => {
                if p == o {
                    return Ok(Value::Bool(true));
                }
                v = p;
            }
        }
    }
}

fn proto_property_is_enumerable(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let key = vm.to_property_key(a.arg(0))?;
    let o = vm.to_object(a.this)?;
    Ok(Value::Bool(match vm.own_property(o, key)? {
        Some(p) => p.attrs.enumerable(),
        None => false,
    }))
}

fn proto_to_locale_string(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    vm.invoke(a.this, PropKey::Str(vm.names.toString), &[])
}

/// Object.prototype.toString.
pub fn proto_to_string(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let this = a.this;
    if this.is_undefined() {
        return Ok(vm.str("[object Undefined]"));
    }
    if this.is_null() {
        return Ok(vm.str("[object Null]"));
    }
    let o = vm.to_object(this)?;
    let builtin_tag: String = if vm.is_array(this)? {
        "Array".into()
    } else {
        match &vm.heap.obj(o).class {
            Class::Arguments(_) => "Arguments".into(),
            Class::Function(_) | Class::Native(_) | Class::Bound(_) => "Function".into(),
            Class::Proxy(p) if p.callable => "Function".into(),
            Class::Error(_) => "Error".into(),
            Class::Boolean(_) => "Boolean".into(),
            Class::Number(_) => "Number".into(),
            Class::String(_) => "String".into(),
            Class::Date(_) => "Date".into(),
            Class::RegExp(_) => "RegExp".into(),
            Class::Host(h) => vm
                .host_classes
                .get(h.class.0 as usize)
                .map(|d| d.name.clone())
                .unwrap_or_else(|| "Object".into()),
            _ => "Object".into(),
        }
    };
    let tag_key = PropKey::Sym(vm.well_known.to_string_tag);
    let tag = vm.get(o, tag_key)?;
    let tag = match tag {
        Value::String(s) => vm.heap.str_to_string(s),
        _ => builtin_tag,
    };
    Ok(vm.str(&format!("[object {tag}]")))
}

fn proto_value_of(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    Ok(Value::Object(vm.to_object(a.this)?))
}

fn proto_define_getter(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = vm.to_object(a.this)?;
    if !vm.is_callable(a.arg(1)) {
        return vm.type_error("Getter must be a function");
    }
    let key = vm.to_property_key(a.arg(0))?;
    let desc = PropertyDescriptor {
        get: Some(a.arg(1)),
        enumerable: Some(true),
        configurable: Some(true),
        ..Default::default()
    };
    vm.define_property_or_throw(o, key, desc)?;
    Ok(Value::Undefined)
}

fn proto_define_setter(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = vm.to_object(a.this)?;
    if !vm.is_callable(a.arg(1)) {
        return vm.type_error("Setter must be a function");
    }
    let key = vm.to_property_key(a.arg(0))?;
    let desc = PropertyDescriptor {
        set: Some(a.arg(1)),
        enumerable: Some(true),
        configurable: Some(true),
        ..Default::default()
    };
    vm.define_property_or_throw(o, key, desc)?;
    Ok(Value::Undefined)
}

fn lookup_accessor(vm: &mut Vm, a: &CallArgs, getter: bool) -> JsResult<Value> {
    let mut o = vm.to_object(a.this)?;
    let key = vm.to_property_key(a.arg(0))?;
    loop {
        if let Some(p) = vm.own_property(o, key)? {
            return Ok(match p.slot {
                PropSlot::Accessor { get, set } => {
                    if getter {
                        get.value()
                    } else {
                        set.value()
                    }
                }
                PropSlot::Data(_) => Value::Undefined,
            });
        }
        match vm.get_prototype_of(o)? {
            Some(p) => o = p,
            None => return Ok(Value::Undefined),
        }
    }
}

fn proto_lookup_getter(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    lookup_accessor(vm, a, true)
}

fn proto_lookup_setter(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    lookup_accessor(vm, a, false)
}

fn proto_get_proto(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = vm.to_object(a.this)?;
    Ok(match vm.get_prototype_of(o)? {
        Some(p) => Value::Object(p),
        None => Value::Null,
    })
}

fn proto_set_proto(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if a.this.is_nullish() {
        return vm.type_error("Object.prototype.__proto__ called on null or undefined");
    }
    let proto = match a.arg(0) {
        Value::Object(p) => Some(p),
        Value::Null => None,
        _ => return Ok(Value::Undefined),
    };
    let Value::Object(o) = a.this else {
        return Ok(Value::Undefined);
    };
    if !vm.set_prototype_of(o, proto)? {
        return vm.type_error("Cannot set prototype");
    }
    Ok(Value::Undefined)
}

/// Attributes helper shared with other builtins.
pub const DATA_ALL: Attrs = Attrs::ALL;
