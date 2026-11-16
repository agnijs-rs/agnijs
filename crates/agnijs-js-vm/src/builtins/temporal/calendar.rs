//! Calendars and the calendar fields of property bags: the
//! specification's ToTemporalCalendarIdentifier,
//! GetTemporalCalendarIdentifierWithISODefault, PrepareCalendarFields and
//! IsPartialTemporalObject. Fields are read in alphabetical order, each
//! with the coercion the specification's field table names.

use agnijs_js_temporal::{Calendar, DateFields, MonthCode};

use super::options::to_integer_with_truncation;
use super::{temporal_data, OrThrow, TemporalData};
use crate::convert::Hint;
use crate::value::{ObjId, Value};
use crate::vm::{JsResult, Vm};

/// The calendar of a Temporal object that has one.
fn calendar_of(vm: &Vm, v: Value) -> Option<Calendar> {
    match temporal_data(vm, v)? {
        TemporalData::PlainDate(d) => Some(d.calendar()),
        _ => None,
    }
}

/// ToTemporalCalendarIdentifier: a Temporal object's calendar, or a
/// calendar identifier or ISO 8601 string. Anything else that is not a
/// string is a `TypeError`.
pub fn to_calendar(vm: &mut Vm, v: Value) -> JsResult<Calendar> {
    if let Some(c) = calendar_of(vm, v) {
        return Ok(c);
    }
    if !matches!(v, Value::String(_)) {
        return vm.type_error("a calendar must be a string or a Temporal object with a calendar");
    }
    let s = vm.to_rust_string(v)?;
    Calendar::parse(&s).or_throw(vm)
}

/// GetTemporalCalendarIdentifierWithISODefault: the bag's `calendar`, the
/// ISO calendar when it is undefined.
pub fn calendar_with_iso_default(vm: &mut Vm, item: ObjId) -> JsResult<Calendar> {
    let v = vm.get_str(item, "calendar")?;
    if v.is_undefined() {
        return Ok(Calendar::iso());
    }
    to_calendar(vm, v)
}

/// ToPositiveIntegerWithTruncation, saturated to `u8`: every month and
/// day above 255 is out of range in every calendar either way.
fn to_positive_u8(vm: &mut Vm, v: Value, name: &str) -> JsResult<u8> {
    let n = to_integer_with_truncation(vm, v)?;
    if n <= 0.0 {
        return vm.range_error(&format!("{name} must be a positive integer"));
    }
    Ok(n.min(255.0) as u8)
}

/// ToIntegerWithTruncation, saturated to `i32`: a year beyond it is out
/// of the representable range and fails when the date is resolved.
fn to_i32(vm: &mut Vm, v: Value) -> JsResult<i32> {
    Ok(to_integer_with_truncation(vm, v)? as i32)
}

/// ToPrimitiveAndRequireString.
fn to_primitive_string(vm: &mut Vm, v: Value, name: &str) -> JsResult<String> {
    let p = vm.to_primitive(v, Hint::String)?;
    if !matches!(p, Value::String(_)) {
        return vm.type_error(&format!("{name} must be a string"));
    }
    vm.to_rust_string(p)
}

/// PrepareCalendarFields for the date fields (`day`, `era`, `eraYear`,
/// `month`, `monthCode`, `year`; `era` and `eraYear` only for a calendar
/// with eras). With `partial`, a bag defining none of them is a
/// `TypeError`; otherwise missing fields are left for the calendar to
/// reject when it resolves the date.
pub fn read_date_fields(vm: &mut Vm, item: ObjId, calendar: &Calendar, partial: bool) -> JsResult<DateFields> {
    let mut f = DateFields::default();
    let v = vm.get_str(item, "day")?;
    if !v.is_undefined() {
        f.day = Some(to_positive_u8(vm, v, "day")?);
    }
    if calendar.has_eras() {
        let v = vm.get_str(item, "era")?;
        if !v.is_undefined() {
            f.era = Some(to_primitive_string(vm, v, "era")?);
        }
        let v = vm.get_str(item, "eraYear")?;
        if !v.is_undefined() {
            f.era_year = Some(to_i32(vm, v)?);
        }
    }
    let v = vm.get_str(item, "month")?;
    if !v.is_undefined() {
        f.month = Some(to_positive_u8(vm, v, "month")?);
    }
    let v = vm.get_str(item, "monthCode")?;
    if !v.is_undefined() {
        let s = to_primitive_string(vm, v, "monthCode")?;
        f.month_code = Some(MonthCode::parse(&s).or_throw(vm)?);
    }
    let v = vm.get_str(item, "year")?;
    if !v.is_undefined() {
        f.year = Some(to_i32(vm, v)?);
    }
    if partial && f.is_empty() {
        return vm.type_error("the object has none of the fields this type needs");
    }
    Ok(f)
}

/// IsPartialTemporalObject: an object that is not a Temporal object and
/// has neither `calendar` nor `timeZone`, or a `TypeError`.
pub fn require_partial_temporal_object(vm: &mut Vm, v: Value) -> JsResult<ObjId> {
    let Value::Object(o) = v else {
        return vm.type_error("an object with the fields to change is required");
    };
    if temporal_data(vm, v).is_some() {
        return vm.type_error("a Temporal object cannot be used as a set of fields to change");
    }
    for name in ["calendar", "timeZone"] {
        if !vm.get_str(o, name)?.is_undefined() {
            return vm.type_error(&format!("{name} cannot be changed with with(); use the matching with method"));
        }
    }
    Ok(o)
}
