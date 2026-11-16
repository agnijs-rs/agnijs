//! Temporal (ECMA-262 since ES2026, the `Temporal` namespace) on the
//! `agnijs-js-temporal` seam (ADR 0027). This module owns what JavaScript
//! can observe: property bags and options read in specification order,
//! coercions, receivers and the order of errors. The date, time, calendar
//! and time zone arithmetic is a call into the seam.

use agnijs_engine_api::RealmId;

use crate::realm::Intrinsic;
use crate::value::{Attrs, Value};
use crate::vm::Vm;

pub fn install(vm: &mut Vm, realm: RealmId) {
    let temporal = vm.new_object();
    vm.realm_mut().set_intrinsic(Intrinsic::Temporal, temporal);
    let global = vm.realm_of(realm).global;
    vm.define_raw_str(global, "Temporal", Value::Object(temporal), Attrs::METHOD);
    super::to_string_tag(vm, temporal, "Temporal");
}
