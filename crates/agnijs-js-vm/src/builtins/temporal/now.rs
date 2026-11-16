//! `Temporal.Now`: the current exact time and the system time zone, both
//! taken from the host (`Host::now_ns`, `Host::default_time_zone`), never
//! from the operating system directly.

use agnijs_js_temporal as seam;

use super::instant::wrap_instant;
use super::OrThrow;
use crate::object::CallArgs;
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, Value};
use crate::vm::{JsResult, Vm};

pub fn install(vm: &mut Vm, namespace: ObjId) {
    let now = vm.new_object();
    vm.realm_mut().set_intrinsic(Intrinsic::TemporalNow, now);
    vm.define_raw_str(namespace, "Now", Value::Object(now), Attrs::METHOD);
    super::super::to_string_tag(vm, now, "Temporal.Now");
    vm.method(now, "instant", 0, instant);
    vm.method(now, "timeZoneId", 0, time_zone_id);
}

/// SystemTimeZoneIdentifier: the host's zone in the database's case, or
/// UTC when the host names a zone the database does not know.
pub fn system_time_zone(vm: &Vm) -> String {
    seam::time_zone_identifier(&vm.host.default_time_zone()).unwrap_or_else(|_| "UTC".to_string())
}

fn instant(vm: &mut Vm, _: &CallArgs) -> JsResult<Value> {
    let ns = vm.host.now_ns();
    let i = seam::Instant::new(ns).or_throw(vm)?;
    wrap_instant(vm, i)
}

fn time_zone_id(vm: &mut Vm, _: &CallArgs) -> JsResult<Value> {
    let id = system_time_zone(vm);
    Ok(vm.str(&id))
}
