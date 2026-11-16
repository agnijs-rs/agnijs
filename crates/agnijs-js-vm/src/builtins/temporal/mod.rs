//! Temporal (ECMA-262 after ES2025, TASK-103.8) on the `agnijs-js-temporal`
//! seam (ADR 0027). This module owns what JavaScript observes: receivers,
//! the order options and property bags are read in, coercions and which
//! error comes first. The seam does the arithmetic.
//!
//! Every Temporal object is `Class::Temporal` with a [`TemporalData`]: a
//! fixed-size value with no references into the heap, so the collector
//! has nothing to trace in it and a snapshot refuses it as it does Intl
//! objects.

pub mod duration;
pub mod options;

use agnijs_engine_api::RealmId;
use agnijs_js_temporal as seam;

use crate::object::{Class, NativeFn, ObjectData};
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, Value};
use crate::vm::{ErrorType, JsResult, Vm};

/// The internal slots of a Temporal object.
pub enum TemporalData {
    Duration(seam::Duration),
}

/// The `TemporalData` of `v` when it is a Temporal object.
pub fn temporal_data(vm: &Vm, v: Value) -> Option<&TemporalData> {
    let o = v.as_object()?;
    match &vm.heap.obj(o).class {
        Class::Temporal(d) => Some(d),
        _ => None,
    }
}

/// OrdinaryCreateFromConstructor for a Temporal object; `new_target`
/// undefined takes the intrinsic prototype.
pub fn create(vm: &mut Vm, new_target: Value, proto: Intrinsic, data: TemporalData) -> JsResult<ObjId> {
    let proto = vm.get_prototype_from_constructor(new_target, proto)?;
    Ok(vm.alloc(ObjectData::new(Some(proto), Class::Temporal(Box::new(data)))))
}

/// Throw a seam error as the JavaScript error its kind names.
pub trait OrThrow<T> {
    fn or_throw(self, vm: &mut Vm) -> JsResult<T>;
}

impl<T> OrThrow<T> for seam::Result<T> {
    fn or_throw(self, vm: &mut Vm) -> JsResult<T> {
        self.map_err(|e| {
            let ty = match e.kind {
                seam::ErrorKind::Range => ErrorType::RangeError,
                seam::ErrorKind::Type => ErrorType::TypeError,
                seam::ErrorKind::Internal => ErrorType::Error,
            };
            vm.throw(ty, &e.message)
        })
    }
}

/// A Temporal constructor on the namespace, its prototype wired both ways
/// and both registered as intrinsics.
#[allow(clippy::too_many_arguments)]
pub fn install_class(
    vm: &mut Vm,
    namespace: ObjId,
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
    vm.define_raw_str(namespace, name, Value::Object(ctor), Attrs::METHOD);
    ctor
}

pub fn install(vm: &mut Vm, realm: RealmId) {
    let temporal = vm.new_object();
    vm.realm_mut().set_intrinsic(Intrinsic::Temporal, temporal);
    let global = vm.realm_of(realm).global;
    vm.define_raw_str(global, "Temporal", Value::Object(temporal), Attrs::METHOD);
    super::to_string_tag(vm, temporal, "Temporal");
    duration::install(vm, temporal);
}
