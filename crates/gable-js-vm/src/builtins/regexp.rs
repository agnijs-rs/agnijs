//! RegExp objects. The matcher (gable-js-regex) and the prototype methods
//! land in M3; this module holds the data model and object creation.

use crate::gc::Tracer;
use crate::object::{CallArgs, Class, ObjectData};
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, PropKey, StrId, Value};
use crate::vm::{JsResult, Vm};

pub struct RegExpData {
    pub source: StrId,
    pub flags: StrId,
}

impl RegExpData {
    pub fn trace(&self, t: &mut Tracer) {
        t.str(self.source);
        t.str(self.flags);
    }
}

pub struct RegExpIterState {
    pub regexp: Value,
    pub string: Value,
    pub global: bool,
    pub unicode: bool,
    pub done: bool,
}

/// RegExpCreate for a literal `/p/f`.
pub fn regexp_create(vm: &mut Vm, pattern: StrId, flags: StrId) -> JsResult<Value> {
    let proto = vm.intrinsic(Intrinsic::RegExpPrototype);
    regexp_alloc(vm, pattern, flags, proto)
}

pub fn regexp_alloc(vm: &mut Vm, pattern: StrId, flags: StrId, proto: ObjId) -> JsResult<Value> {
    let data = RegExpData {
        source: pattern,
        flags,
    };
    let o = vm.alloc(ObjectData::new(Some(proto), Class::RegExp(Box::new(data))));
    let key = PropKey::Str(vm.names.lastIndex);
    vm.define_raw(o, key, Value::Number(0.0), Attrs::WRITABLE_ONLY);
    Ok(Value::Object(o))
}

pub fn install(vm: &mut Vm, realm: gable_engine_api::RealmId) {
    let proto = vm.new_object();
    vm.realm_mut()
        .set_intrinsic(Intrinsic::RegExpPrototype, proto);
    let ctor = vm.new_native_ctor("RegExp", 2, regexp_constructor);
    vm.realm_mut().set_intrinsic(Intrinsic::RegExp, ctor);
    vm.define_raw_str(ctor, "prototype", Value::Object(proto), Attrs::NONE);
    vm.define_raw_str(proto, "constructor", Value::Object(ctor), Attrs::METHOD);
    let global = vm.realm_of(realm).global;
    vm.define_raw_str(global, "RegExp", Value::Object(ctor), Attrs::METHOD);
}

fn regexp_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let proto = vm.get_prototype_from_constructor(a.new_target, Intrinsic::RegExpPrototype)?;
    let p = if a.arg(0).is_undefined() {
        vm.names.empty
    } else {
        vm.to_string(a.arg(0))?
    };
    let f = if a.arg(1).is_undefined() {
        vm.names.empty
    } else {
        vm.to_string(a.arg(1))?
    };
    regexp_alloc(vm, p, f, proto)
}

/// RegExpCreate(P, F) with Value arguments (String.prototype.match etc.).
pub fn regexp_create_from_value(vm: &mut Vm, pattern: Value, flags: Value) -> JsResult<Value> {
    let p = if pattern.is_undefined() {
        vm.names.empty
    } else {
        vm.to_string(pattern)?
    };
    let f = if flags.is_undefined() {
        vm.names.empty
    } else {
        vm.to_string(flags)?
    };
    regexp_create(vm, p, f)
}
