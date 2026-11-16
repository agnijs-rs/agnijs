//! `Temporal.PlainTime`.

use agnijs_js_temporal::{self as seam, Overflow, TimeFields, ToStringOptions, Unit};

use super::calendar::require_partial_temporal_object;
use super::duration::{to_temporal_duration, wrap_duration};
use super::instant::{read_round_to, require_time_unit, time_difference_settings};
use super::options::{
    get_fractional_second_digits, get_options_object, get_overflow, get_rounding_mode, get_unit,
    to_integer_with_truncation,
};
use super::{create, install_class, temporal_data, OrThrow, TemporalData};
use crate::object::CallArgs;
use crate::realm::Intrinsic;
use crate::value::{ObjId, Value};
use crate::vm::{JsResult, Vm};

pub fn install(vm: &mut Vm, namespace: ObjId) {
    let proto = vm.new_object();
    let ctor = install_class(
        vm,
        namespace,
        "PlainTime",
        0,
        construct,
        proto,
        Intrinsic::TemporalPlainTime,
        Intrinsic::TemporalPlainTimePrototype,
    );
    vm.method(ctor, "from", 1, from);
    vm.method(ctor, "compare", 2, compare);

    vm.accessor(proto, "hour", get_hour, None);
    vm.accessor(proto, "minute", get_minute, None);
    vm.accessor(proto, "second", get_second, None);
    vm.accessor(proto, "millisecond", get_millisecond, None);
    vm.accessor(proto, "microsecond", get_microsecond, None);
    vm.accessor(proto, "nanosecond", get_nanosecond, None);
    vm.method(proto, "with", 1, with);
    vm.method(proto, "add", 1, add);
    vm.method(proto, "subtract", 1, subtract);
    vm.method(proto, "until", 1, until);
    vm.method(proto, "since", 1, since);
    vm.method(proto, "round", 1, round);
    vm.method(proto, "equals", 1, equals);
    vm.method(proto, "toString", 0, to_string);
    vm.method(proto, "toLocaleString", 0, to_locale_string);
    vm.method(proto, "toJSON", 0, to_json);
    vm.method(proto, "valueOf", 0, value_of);
    super::super::to_string_tag(vm, proto, "Temporal.PlainTime");
}

/// RequireInternalSlot(this, [[InitializedTemporalTime]]).
fn this_time(vm: &mut Vm, this: Value, method: &str) -> JsResult<seam::PlainTime> {
    match temporal_data(vm, this) {
        Some(TemporalData::PlainTime(t)) => Ok(*t),
        _ => vm.type_error(&format!("Temporal.PlainTime.prototype.{method} called on an incompatible receiver")),
    }
}

pub fn wrap_plain_time(vm: &mut Vm, t: seam::PlainTime) -> JsResult<Value> {
    let o = create(vm, Value::Undefined, Intrinsic::TemporalPlainTimePrototype, TemporalData::PlainTime(t))?;
    Ok(Value::Object(o))
}

fn read_overflow(vm: &mut Vm, options: Value) -> JsResult<Overflow> {
    let bag = get_options_object(vm, options)?;
    get_overflow(vm, bag)
}

/// ToIntegerWithTruncation saturated to `i64`; every value beyond it is
/// out of every field's range alike.
fn to_i64(vm: &mut Vm, v: Value) -> JsResult<i64> {
    Ok(to_integer_with_truncation(vm, v)? as i64)
}

/// ToTemporalTimeRecord: the time fields in alphabetical order, each
/// ToIntegerWithTruncation; a `TypeError` when the bag defines none.
pub fn read_time_fields(vm: &mut Vm, item: ObjId) -> JsResult<TimeFields> {
    let mut f = TimeFields::default();
    let slots: [(&str, fn(&mut TimeFields) -> &mut Option<i64>); 6] = [
        ("hour", |f| &mut f.hour),
        ("microsecond", |f| &mut f.microsecond),
        ("millisecond", |f| &mut f.millisecond),
        ("minute", |f| &mut f.minute),
        ("nanosecond", |f| &mut f.nanosecond),
        ("second", |f| &mut f.second),
    ];
    for (name, slot) in slots {
        let v = vm.get_str(item, name)?;
        if !v.is_undefined() {
            *slot(&mut f) = Some(to_i64(vm, v)?);
        }
    }
    if f.is_empty() {
        return vm.type_error("a time-like object needs at least one time property");
    }
    Ok(f)
}

/// `new Temporal.PlainTime(hour, minute, second, millisecond,
/// microsecond, nanosecond)`, each defaulting to zero.
fn construct(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if a.new_target.is_undefined() {
        return vm.type_error("Temporal.PlainTime must be called with new");
    }
    let mut f = [0i64; 6];
    for (i, slot) in f.iter_mut().enumerate() {
        let v = a.arg(i);
        if !v.is_undefined() {
            *slot = to_i64(vm, v)?;
        }
    }
    let t = seam::PlainTime::new(f).or_throw(vm)?;
    let o = create(vm, a.new_target, Intrinsic::TemporalPlainTimePrototype, TemporalData::PlainTime(t))?;
    Ok(Value::Object(o))
}

/// ToTemporalTime. Plain date-times and zoned date-times convert when
/// those types exist (TASK-103.8.6, TASK-103.8.8).
pub fn to_temporal_time(vm: &mut Vm, item: Value, options: Value) -> JsResult<seam::PlainTime> {
    if let Value::Object(o) = item {
        if let Some(TemporalData::PlainTime(t)) = temporal_data(vm, item) {
            let t = *t;
            read_overflow(vm, options)?;
            return Ok(t);
        }
        let fields = read_time_fields(vm, o)?;
        let overflow = read_overflow(vm, options)?;
        return seam::PlainTime::from_fields(&fields, overflow).or_throw(vm);
    }
    if !matches!(item, Value::String(_)) {
        return vm.type_error("a Temporal.PlainTime, an ISO 8601 time string or a time-like object is required");
    }
    let s = vm.to_rust_string(item)?;
    let t = seam::PlainTime::parse(&s).or_throw(vm)?;
    read_overflow(vm, options)?;
    Ok(t)
}

fn from(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let t = to_temporal_time(vm, a.arg(0), a.arg(1))?;
    wrap_plain_time(vm, t)
}

fn compare(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let one = to_temporal_time(vm, a.arg(0), Value::Undefined)?;
    let two = to_temporal_time(vm, a.arg(1), Value::Undefined)?;
    Ok(Value::Number(one.compare(&two) as i8 as f64))
}

/// A getter returning one field of the time.
macro_rules! field_getters {
    ($($name:ident, $js:literal, $f:ident;)*) => {$(
        fn $name(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
            let t = this_time(vm, a.this, $js)?;
            Ok(Value::Number(f64::from(t.$f())))
        }
    )*};
}

field_getters! {
    get_hour, "hour", hour;
    get_minute, "minute", minute;
    get_second, "second", second;
    get_millisecond, "millisecond", millisecond;
    get_microsecond, "microsecond", microsecond;
    get_nanosecond, "nanosecond", nanosecond;
}

fn with(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let t = this_time(vm, a.this, "with")?;
    let like = require_partial_temporal_object(vm, a.arg(0))?;
    let fields = read_time_fields(vm, like)?;
    let overflow = read_overflow(vm, a.arg(1))?;
    let r = t.with(&fields, overflow).or_throw(vm)?;
    wrap_plain_time(vm, r)
}

fn add(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let t = this_time(vm, a.this, "add")?;
    let d = to_temporal_duration(vm, a.arg(0))?;
    let r = t.add(&d).or_throw(vm)?;
    wrap_plain_time(vm, r)
}

fn subtract(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let t = this_time(vm, a.this, "subtract")?;
    let d = to_temporal_duration(vm, a.arg(0))?;
    let r = t.subtract(&d).or_throw(vm)?;
    wrap_plain_time(vm, r)
}

fn until(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let t = this_time(vm, a.this, "until")?;
    let other = to_temporal_time(vm, a.arg(0), Value::Undefined)?;
    let settings = time_difference_settings(vm, a.arg(1))?;
    let d = t.until(&other, settings).or_throw(vm)?;
    wrap_duration(vm, d)
}

fn since(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let t = this_time(vm, a.this, "since")?;
    let other = to_temporal_time(vm, a.arg(0), Value::Undefined)?;
    let settings = time_difference_settings(vm, a.arg(1))?;
    let d = t.since(&other, settings).or_throw(vm)?;
    wrap_duration(vm, d)
}

fn round(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let t = this_time(vm, a.this, "round")?;
    let options = read_round_to(vm, a.arg(0), "Temporal.PlainTime.prototype.round")?;
    let r = t.round(options).or_throw(vm)?;
    wrap_plain_time(vm, r)
}

fn equals(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let t = this_time(vm, a.this, "equals")?;
    let other = to_temporal_time(vm, a.arg(0), Value::Undefined)?;
    Ok(Value::Bool(t == other))
}

fn to_string(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let t = this_time(vm, a.this, "toString")?;
    let bag = get_options_object(vm, a.arg(0))?;
    let precision = get_fractional_second_digits(vm, bag)?;
    let rounding_mode = get_rounding_mode(vm, bag)?;
    let smallest_unit = get_unit(vm, bag, "smallestUnit")?;
    if smallest_unit == Some(Unit::Hour) {
        return vm.range_error("smallestUnit cannot be hour");
    }
    require_time_unit(vm, smallest_unit, "smallestUnit", false)?;
    let s = t.to_string_with(ToStringOptions { precision, rounding_mode, smallest_unit }).or_throw(vm)?;
    Ok(vm.str(&s))
}

fn to_json(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let t = this_time(vm, a.this, "toJSON")?;
    let s = t.to_string_with(ToStringOptions::default()).or_throw(vm)?;
    Ok(vm.str(&s))
}

/// `toString()` until Intl formats Temporal objects (TASK-103.8.9).
fn to_locale_string(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let t = this_time(vm, a.this, "toLocaleString")?;
    let s = t.to_string_with(ToStringOptions::default()).or_throw(vm)?;
    Ok(vm.str(&s))
}

fn value_of(vm: &mut Vm, _: &CallArgs) -> JsResult<Value> {
    vm.type_error("Temporal.PlainTime cannot be converted to a primitive; use Temporal.PlainTime.compare")
}
