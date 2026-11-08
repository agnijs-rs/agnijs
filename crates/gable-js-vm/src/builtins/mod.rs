//! The standard library. `install` populates a fresh realm's intrinsics and
//! global object in dependency order.

pub mod array;
pub mod bigint;
pub mod boolean;
pub mod collections;
pub mod arraybuffer;
pub mod atomics;
pub mod date;
pub mod error;
pub mod function;
pub mod generator;
pub mod global;
pub mod iterator;
pub mod iterator_helpers;
pub mod json;
pub mod math;
pub mod number;
pub mod object;
pub mod promise;
pub mod proxy;
pub mod reflect;
pub mod regexp;
pub mod string;
pub mod symbol;
pub mod typedarray;
pub mod typedarray_ctor;

use crate::object::NativeFn;
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, PropKey, Value};
use crate::vm::Vm;
use gable_engine_api::RealmId;

pub fn install(vm: &mut Vm, realm: RealmId) {
    // %Function.prototype% is itself callable and everything else hangs off it.
    let fp = vm.new_native("", 0, function::function_prototype_call);
    vm.realm_mut()
        .set_intrinsic(Intrinsic::FunctionPrototype, fp);

    object::install(vm, realm);
    function::install(vm, realm);
    error::install(vm, realm);
    symbol::install(vm, realm);
    iterator::install(vm, realm);
    iterator_helpers::install(vm, realm);
    generator::install(vm, realm);
    array::install(vm, realm);
    string::install(vm, realm);
    number::install(vm, realm);
    boolean::install(vm, realm);
    bigint::install(vm, realm);
    math::install(vm, realm);
    json::install(vm, realm);
    reflect::install(vm, realm);
    promise::install(vm, realm);
    regexp::install(vm, realm);
    proxy::install(vm, realm);
    collections::install(vm, realm);
    date::install(vm, realm);
    typedarray::install(vm, realm);
    global::install(vm, realm);
}

/// Create a constructor with its prototype wired both ways, register the
/// intrinsics and define the global binding. Returns `(ctor, proto)`.
pub fn install_ctor(
    vm: &mut Vm,
    realm: RealmId,
    name: &str,
    length: u32,
    f: NativeFn,
    proto: ObjId,
    ctor_intrinsic: Intrinsic,
    proto_intrinsic: Intrinsic,
) -> ObjId {
    let ctor = vm.new_native_ctor(name, length, f);
    vm.realm_mut().set_intrinsic(ctor_intrinsic, ctor);
    vm.realm_mut().set_intrinsic(proto_intrinsic, proto);
    vm.define_raw_str(ctor, "prototype", Value::Object(proto), Attrs::NONE);
    vm.define_raw_str(proto, "constructor", Value::Object(ctor), Attrs::METHOD);
    let global = vm.realm_of(realm).global;
    vm.define_raw_str(global, name, Value::Object(ctor), Attrs::METHOD);
    ctor
}

/// Define `@@toStringTag` on `obj`.
pub fn to_string_tag(vm: &mut Vm, obj: ObjId, tag: &str) {
    let v = vm.str(tag);
    let key = PropKey::Sym(vm.well_known.to_string_tag);
    vm.define_raw(obj, key, v, Attrs::CONFIGURABLE_ONLY);
}

/// Define `get [Symbol.species]` returning `this`.
pub fn species_accessor(vm: &mut Vm, ctor: ObjId) {
    let sym = vm.well_known.species;
    vm.accessor_sym(ctor, sym, "Symbol.species", promise::species_getter);
}
