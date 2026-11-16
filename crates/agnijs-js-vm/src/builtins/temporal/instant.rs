//! `Temporal.Instant` and `Date.prototype.toTemporalInstant`.

use agnijs_js_temporal::{self as seam, RoundingOptions, ToStringOptions, Unit};
use num_bigint::BigInt;

use super::duration::{to_temporal_duration, unit_from_string, wrap_duration};
use super::options::{
    get, get_fractional_second_digits, get_options_object, get_rounding_increment, get_rounding_mode, get_unit,
};
use super::{create, install_class, temporal_data, OrThrow, TemporalData};
use crate::convert::Hint;
use crate::object::CallArgs;
use crate::realm::Intrinsic;
use crate::value::{ObjId, Value};
use crate::vm::{JsResult, Vm};

/// The largest epoch milliseconds an instant can hold (10⁸ days).
const MAX_EPOCH_MS: f64 = 8.64e15;

pub fn install(vm: &mut Vm, namespace: ObjId) {
    let proto = vm.new_object();
    let ctor = install_class(
        vm,
        namespace,
        "Instant",
        1,
        construct,
        proto,
        Intrinsic::TemporalInstant,
        Intrinsic::TemporalInstantPrototype,
    );
    vm.method(ctor, "from", 1, from);
    vm.method(ctor, "fromEpochMilliseconds", 1, from_epoch_milliseconds);
    vm.method(ctor, "fromEpochNanoseconds", 1, from_epoch_nanoseconds);
    vm.method(ctor, "compare", 2, compare);

    vm.accessor(proto, "epochMilliseconds", get_epoch_milliseconds, None);
    vm.accessor(proto, "epochNanoseconds", get_epoch_nanoseconds, None);
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
    super::super::to_string_tag(vm, proto, "Temporal.Instant");

    let date_proto = vm.intrinsic(Intrinsic::DatePrototype);
    vm.method(date_proto, "toTemporalInstant", 0, date_to_temporal_instant);
}

/// RequireInternalSlot(this, [[InitializedTemporalInstant]]).
fn this_instant(vm: &mut Vm, this: Value, method: &str) -> JsResult<seam::Instant> {
    match temporal_data(vm, this) {
        Some(TemporalData::Instant(i)) => Ok(*i),
        _ => vm.type_error(&format!("Temporal.Instant.prototype.{method} called on an incompatible receiver")),
    }
}

pub fn wrap_instant(vm: &mut Vm, i: seam::Instant) -> JsResult<Value> {
    let o = create(vm, Value::Undefined, Intrinsic::TemporalInstantPrototype, TemporalData::Instant(i))?;
    Ok(Value::Object(o))
}

/// CreateTemporalInstant from a BigInt: a `RangeError` outside the range.
fn instant_from_bigint(vm: &mut Vm, v: Value) -> JsResult<seam::Instant> {
    let id = vm.to_bigint(v)?;
    match i128::try_from(&vm.heap.bigint(id).0) {
        Ok(ns) => seam::Instant::new(ns).or_throw(vm),
        Err(_) => vm.range_error("the epoch nanoseconds are out of range"),
    }
}

/// `new Temporal.Instant(epochNanoseconds)`.
fn construct(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if a.new_target.is_undefined() {
        return vm.type_error("Temporal.Instant must be called with new");
    }
    let i = instant_from_bigint(vm, a.arg(0))?;
    let o = create(vm, a.new_target, Intrinsic::TemporalInstantPrototype, TemporalData::Instant(i))?;
    Ok(Value::Object(o))
}

/// ToTemporalInstant.
pub fn to_temporal_instant(vm: &mut Vm, item: Value) -> JsResult<seam::Instant> {
    let mut item = item;
    if let Value::Object(_) = item {
        if let Some(TemporalData::Instant(i)) = temporal_data(vm, item) {
            return Ok(*i);
        }
        item = vm.to_primitive(item, Hint::String)?;
    }
    if !matches!(item, Value::String(_)) {
        return vm.type_error("a Temporal.Instant or an ISO 8601 string with an offset is required");
    }
    let s = vm.to_rust_string(item)?;
    seam::Instant::parse(&s).or_throw(vm)
}

/// The epoch milliseconds of a Date's time value, or of
/// `fromEpochMilliseconds`'s argument, as an instant (NumberToBigInt,
/// then CreateTemporalInstant).
fn instant_from_epoch_ms(vm: &mut Vm, ms: f64) -> JsResult<seam::Instant> {
    if !ms.is_finite() || ms.trunc() != ms {
        return vm.range_error("the epoch milliseconds must be an integer");
    }
    if ms.abs() > MAX_EPOCH_MS {
        return vm.range_error("the epoch milliseconds are out of range");
    }
    seam::Instant::new(ms as i128 * 1_000_000).or_throw(vm)
}

fn from(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let i = to_temporal_instant(vm, a.arg(0))?;
    wrap_instant(vm, i)
}

fn from_epoch_milliseconds(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let ms = vm.to_number(a.arg(0))?;
    let i = instant_from_epoch_ms(vm, ms)?;
    wrap_instant(vm, i)
}

fn from_epoch_nanoseconds(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let i = instant_from_bigint(vm, a.arg(0))?;
    wrap_instant(vm, i)
}

fn compare(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let one = to_temporal_instant(vm, a.arg(0))?;
    let two = to_temporal_instant(vm, a.arg(1))?;
    let order = one.epoch_nanoseconds().cmp(&two.epoch_nanoseconds());
    Ok(Value::Number(order as i8 as f64))
}

fn get_epoch_milliseconds(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let i = this_instant(vm, a.this, "epochMilliseconds")?;
    Ok(Value::Number(i.epoch_milliseconds() as f64))
}

fn get_epoch_nanoseconds(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let i = this_instant(vm, a.this, "epochNanoseconds")?;
    Ok(vm.bigint_value(BigInt::from(i.epoch_nanoseconds())))
}

fn add(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let i = this_instant(vm, a.this, "add")?;
    let d = to_temporal_duration(vm, a.arg(0))?;
    let r = i.add(&d).or_throw(vm)?;
    wrap_instant(vm, r)
}

fn subtract(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let i = this_instant(vm, a.this, "subtract")?;
    let d = to_temporal_duration(vm, a.arg(0))?;
    let r = i.subtract(&d).or_throw(vm)?;
    wrap_instant(vm, r)
}

/// A unit that must be a time unit (ValidateTemporalUnitValue with the
/// time group): a `RangeError` for a calendar unit, and for `auto` unless
/// it is allowed.
fn require_time_unit(vm: &mut Vm, unit: Option<Unit>, name: &str, auto_allowed: bool) -> JsResult<()> {
    match unit {
        Some(Unit::Auto) if !auto_allowed => vm.range_error(&format!("{name} cannot be auto")),
        Some(u) if u != Unit::Auto && !u.is_time() => vm.range_error(&format!("{name} must be a time unit")),
        _ => Ok(()),
    }
}

/// GetDifferenceSettings for an instant: the options in alphabetical
/// order, then each validated against the time units.
fn difference_settings(vm: &mut Vm, options: Value) -> JsResult<RoundingOptions> {
    let bag = get_options_object(vm, options)?;
    let settings = RoundingOptions {
        largest_unit: get_unit(vm, bag, "largestUnit")?,
        increment: get_rounding_increment(vm, bag)?,
        rounding_mode: get_rounding_mode(vm, bag)?,
        smallest_unit: get_unit(vm, bag, "smallestUnit")?,
    };
    require_time_unit(vm, settings.largest_unit, "largestUnit", true)?;
    require_time_unit(vm, settings.smallest_unit, "smallestUnit", false)?;
    Ok(settings)
}

fn until(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let i = this_instant(vm, a.this, "until")?;
    let other = to_temporal_instant(vm, a.arg(0))?;
    let settings = difference_settings(vm, a.arg(1))?;
    let d = i.until(&other, settings).or_throw(vm)?;
    wrap_duration(vm, d)
}

fn since(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let i = this_instant(vm, a.this, "since")?;
    let other = to_temporal_instant(vm, a.arg(0))?;
    let settings = difference_settings(vm, a.arg(1))?;
    let d = i.since(&other, settings).or_throw(vm)?;
    wrap_duration(vm, d)
}

fn round(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let i = this_instant(vm, a.this, "round")?;
    let round_to = a.arg(0);
    let mut options = RoundingOptions::default();
    match round_to {
        Value::Undefined => return vm.type_error("Temporal.Instant.prototype.round requires options"),
        Value::String(_) => options.smallest_unit = Some(unit_from_string(vm, round_to, "smallestUnit")?),
        _ => {
            let bag = get_options_object(vm, round_to)?;
            options.increment = get_rounding_increment(vm, bag)?;
            options.rounding_mode = get_rounding_mode(vm, bag)?;
            options.smallest_unit = get_unit(vm, bag, "smallestUnit")?;
        }
    }
    if options.smallest_unit.is_none() {
        return vm.range_error("smallestUnit is required");
    }
    require_time_unit(vm, options.smallest_unit, "smallestUnit", false)?;
    let r = i.round(options).or_throw(vm)?;
    wrap_instant(vm, r)
}

fn equals(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let i = this_instant(vm, a.this, "equals")?;
    let other = to_temporal_instant(vm, a.arg(0))?;
    Ok(Value::Bool(i == other))
}

/// ToTemporalTimeZoneIdentifier for a value that is not undefined.
fn to_time_zone(vm: &mut Vm, v: Value) -> JsResult<seam::TimeZone> {
    // A Temporal.ZonedDateTime gives its own zone (TASK-103.8.8).
    if !matches!(v, Value::String(_)) {
        return vm.type_error("a time zone must be a string");
    }
    let s = vm.to_rust_string(v)?;
    seam::TimeZone::parse(&s).or_throw(vm)
}

fn to_string(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let i = this_instant(vm, a.this, "toString")?;
    let bag = get_options_object(vm, a.arg(0))?;
    let precision = get_fractional_second_digits(vm, bag)?;
    let rounding_mode = get_rounding_mode(vm, bag)?;
    let smallest_unit = get_unit(vm, bag, "smallestUnit")?;
    let time_zone = get(vm, bag, "timeZone")?;
    if smallest_unit == Some(Unit::Hour) {
        return vm.range_error("smallestUnit cannot be hour");
    }
    require_time_unit(vm, smallest_unit, "smallestUnit", false)?;
    let time_zone = match time_zone {
        Value::Undefined => None,
        v => Some(to_time_zone(vm, v)?),
    };
    let options = ToStringOptions { precision, rounding_mode, smallest_unit };
    let s = i.to_string_with(options, time_zone.as_ref()).or_throw(vm)?;
    Ok(vm.str(&s))
}

fn to_json(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let i = this_instant(vm, a.this, "toJSON")?;
    let s = i.to_string_with(ToStringOptions::default(), None).or_throw(vm)?;
    Ok(vm.str(&s))
}

/// `toString()` until Intl formats Temporal objects (TASK-103.8.9).
fn to_locale_string(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let i = this_instant(vm, a.this, "toLocaleString")?;
    let s = i.to_string_with(ToStringOptions::default(), None).or_throw(vm)?;
    Ok(vm.str(&s))
}

fn value_of(vm: &mut Vm, _: &CallArgs) -> JsResult<Value> {
    vm.type_error("Temporal.Instant cannot be converted to a primitive; use Temporal.Instant.compare")
}

/// `Date.prototype.toTemporalInstant()`.
fn date_to_temporal_instant(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (_, t) = crate::builtins::date::this_time_value(vm, a.this, "toTemporalInstant")?;
    let i = instant_from_epoch_ms(vm, t)?;
    wrap_instant(vm, i)
}
