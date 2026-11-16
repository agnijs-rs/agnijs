//! `Temporal.PlainDate`.

use agnijs_js_temporal::{self as seam, Calendar, Overflow, RoundingOptions, Unit};

use super::calendar::{calendar_with_iso_default, read_date_fields, require_partial_temporal_object, to_calendar};
use super::duration::{to_temporal_duration, wrap_duration};
use super::options::{
    get_options_object, get_overflow, get_rounding_increment, get_rounding_mode, get_show_calendar, get_unit,
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
        "PlainDate",
        3,
        construct,
        proto,
        Intrinsic::TemporalPlainDate,
        Intrinsic::TemporalPlainDatePrototype,
    );
    vm.method(ctor, "from", 1, from);
    vm.method(ctor, "compare", 2, compare);

    vm.accessor(proto, "calendarId", get_calendar_id, None);
    vm.accessor(proto, "era", get_era, None);
    vm.accessor(proto, "eraYear", get_era_year, None);
    vm.accessor(proto, "year", get_year, None);
    vm.accessor(proto, "month", get_month, None);
    vm.accessor(proto, "monthCode", get_month_code, None);
    vm.accessor(proto, "day", get_day, None);
    vm.accessor(proto, "dayOfWeek", get_day_of_week, None);
    vm.accessor(proto, "dayOfYear", get_day_of_year, None);
    vm.accessor(proto, "weekOfYear", get_week_of_year, None);
    vm.accessor(proto, "yearOfWeek", get_year_of_week, None);
    vm.accessor(proto, "daysInWeek", get_days_in_week, None);
    vm.accessor(proto, "daysInMonth", get_days_in_month, None);
    vm.accessor(proto, "daysInYear", get_days_in_year, None);
    vm.accessor(proto, "monthsInYear", get_months_in_year, None);
    vm.accessor(proto, "inLeapYear", get_in_leap_year, None);
    vm.method(proto, "with", 1, with);
    vm.method(proto, "withCalendar", 1, with_calendar);
    vm.method(proto, "add", 1, add);
    vm.method(proto, "subtract", 1, subtract);
    vm.method(proto, "until", 1, until);
    vm.method(proto, "since", 1, since);
    vm.method(proto, "equals", 1, equals);
    vm.method(proto, "toString", 0, to_string);
    vm.method(proto, "toLocaleString", 0, to_locale_string);
    vm.method(proto, "toJSON", 0, to_json);
    vm.method(proto, "valueOf", 0, value_of);
    super::super::to_string_tag(vm, proto, "Temporal.PlainDate");
}

/// RequireInternalSlot(this, [[InitializedTemporalDate]]).
fn this_date(vm: &mut Vm, this: Value, method: &str) -> JsResult<seam::PlainDate> {
    match temporal_data(vm, this) {
        Some(TemporalData::PlainDate(d)) => Ok(d.clone()),
        _ => vm.type_error(&format!("Temporal.PlainDate.prototype.{method} called on an incompatible receiver")),
    }
}

pub fn wrap_plain_date(vm: &mut Vm, d: seam::PlainDate) -> JsResult<Value> {
    let o = create(vm, Value::Undefined, Intrinsic::TemporalPlainDatePrototype, TemporalData::PlainDate(d))?;
    Ok(Value::Object(o))
}

/// GetOptionsObject followed by GetTemporalOverflowOption.
fn read_overflow(vm: &mut Vm, options: Value) -> JsResult<Overflow> {
    let bag = get_options_object(vm, options)?;
    get_overflow(vm, bag)
}

/// `new Temporal.PlainDate(isoYear, isoMonth, isoDay [, calendar])`.
fn construct(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if a.new_target.is_undefined() {
        return vm.type_error("Temporal.PlainDate must be called with new");
    }
    let y = to_integer_with_truncation(vm, a.arg(0))?;
    let m = to_integer_with_truncation(vm, a.arg(1))?;
    let d = to_integer_with_truncation(vm, a.arg(2))?;
    let calendar = match a.arg(3) {
        Value::Undefined => Calendar::iso(),
        v @ Value::String(_) => {
            let s = vm.to_rust_string(v)?;
            Calendar::from_identifier(&s).or_throw(vm)?
        }
        _ => return vm.type_error("the calendar must be a string"),
    };
    // Saturating: anything outside these types is not a valid ISO date.
    let date =
        seam::PlainDate::new(y as i32, m.clamp(0.0, 255.0) as u8, d.clamp(0.0, 255.0) as u8, calendar).or_throw(vm)?;
    let o = create(vm, a.new_target, Intrinsic::TemporalPlainDatePrototype, TemporalData::PlainDate(date))?;
    Ok(Value::Object(o))
}

/// ToTemporalDate. Plain date-times and zoned date-times convert when
/// those types exist (TASK-103.8.6, TASK-103.8.8).
pub fn to_temporal_date(vm: &mut Vm, item: Value, options: Value) -> JsResult<seam::PlainDate> {
    if let Value::Object(o) = item {
        if let Some(TemporalData::PlainDate(d)) = temporal_data(vm, item) {
            let d = d.clone();
            read_overflow(vm, options)?;
            return Ok(d);
        }
        let calendar = calendar_with_iso_default(vm, o)?;
        let fields = read_date_fields(vm, o, &calendar, false)?;
        let overflow = read_overflow(vm, options)?;
        return seam::PlainDate::from_fields(&fields, calendar, overflow).or_throw(vm);
    }
    if !matches!(item, Value::String(_)) {
        return vm.type_error("a Temporal.PlainDate, an ISO 8601 date string or a date-like object is required");
    }
    let s = vm.to_rust_string(item)?;
    let d = seam::PlainDate::parse(&s).or_throw(vm)?;
    read_overflow(vm, options)?;
    Ok(d)
}

fn from(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = to_temporal_date(vm, a.arg(0), a.arg(1))?;
    wrap_plain_date(vm, d)
}

fn compare(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let one = to_temporal_date(vm, a.arg(0), Value::Undefined)?;
    let two = to_temporal_date(vm, a.arg(1), Value::Undefined)?;
    Ok(Value::Number(one.compare(&two) as i8 as f64))
}

fn get_calendar_id(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_date(vm, a.this, "calendarId")?;
    Ok(vm.str(d.calendar().identifier()))
}

fn get_era(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_date(vm, a.this, "era")?;
    Ok(match d.era() {
        Some(e) => vm.str(&e),
        None => Value::Undefined,
    })
}

fn get_era_year(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_date(vm, a.this, "eraYear")?;
    Ok(d.era_year().map_or(Value::Undefined, |y| Value::Number(f64::from(y))))
}

/// A getter returning a number computed from the date.
macro_rules! number_getters {
    ($($name:ident, $js:literal, $f:expr;)*) => {$(
        fn $name(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
            let d = this_date(vm, a.this, $js)?;
            let f: fn(&seam::PlainDate) -> f64 = $f;
            Ok(Value::Number(f(&d)))
        }
    )*};
}

number_getters! {
    get_year, "year", |d| f64::from(d.year());
    get_month, "month", |d| f64::from(d.month());
    get_day, "day", |d| f64::from(d.day());
    get_day_of_week, "dayOfWeek", |d| f64::from(d.day_of_week());
    get_day_of_year, "dayOfYear", |d| f64::from(d.day_of_year());
    get_days_in_week, "daysInWeek", |d| f64::from(d.days_in_week());
    get_days_in_month, "daysInMonth", |d| f64::from(d.days_in_month());
    get_days_in_year, "daysInYear", |d| f64::from(d.days_in_year());
    get_months_in_year, "monthsInYear", |d| f64::from(d.months_in_year());
}

fn get_month_code(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_date(vm, a.this, "monthCode")?;
    Ok(vm.str(d.month_code().as_str()))
}

fn get_week_of_year(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_date(vm, a.this, "weekOfYear")?;
    Ok(d.week_of_year().map_or(Value::Undefined, |w| Value::Number(f64::from(w))))
}

fn get_year_of_week(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_date(vm, a.this, "yearOfWeek")?;
    Ok(d.year_of_week().map_or(Value::Undefined, |y| Value::Number(f64::from(y))))
}

fn get_in_leap_year(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_date(vm, a.this, "inLeapYear")?;
    Ok(Value::Bool(d.in_leap_year()))
}

fn with(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_date(vm, a.this, "with")?;
    let like = require_partial_temporal_object(vm, a.arg(0))?;
    let fields = read_date_fields(vm, like, &d.calendar(), true)?;
    let overflow = read_overflow(vm, a.arg(1))?;
    let r = d.with(&fields, overflow).or_throw(vm)?;
    wrap_plain_date(vm, r)
}

fn with_calendar(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_date(vm, a.this, "withCalendar")?;
    if a.arg(0).is_undefined() {
        return vm.type_error("a calendar is required");
    }
    let calendar = to_calendar(vm, a.arg(0))?;
    wrap_plain_date(vm, d.with_calendar(calendar))
}

fn add(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_date(vm, a.this, "add")?;
    let duration = to_temporal_duration(vm, a.arg(0))?;
    let overflow = read_overflow(vm, a.arg(1))?;
    let r = d.add(&duration, overflow).or_throw(vm)?;
    wrap_plain_date(vm, r)
}

fn subtract(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_date(vm, a.this, "subtract")?;
    let duration = to_temporal_duration(vm, a.arg(0))?;
    let overflow = read_overflow(vm, a.arg(1))?;
    let r = d.subtract(&duration, overflow).or_throw(vm)?;
    wrap_plain_date(vm, r)
}

/// A unit that must be a date unit (ValidateTemporalUnitValue with the
/// date group): a `RangeError` for a time unit, and for `auto` unless it
/// is allowed.
fn require_date_unit(vm: &mut Vm, unit: Option<Unit>, name: &str, auto_allowed: bool) -> JsResult<()> {
    match unit {
        Some(Unit::Auto) if !auto_allowed => vm.range_error(&format!("{name} cannot be auto")),
        Some(u) if u != Unit::Auto && !u.is_date() => vm.range_error(&format!("{name} must be a date unit")),
        _ => Ok(()),
    }
}

/// DifferenceTemporalPlainDate up to the arithmetic: the other date, the
/// calendar check, then GetDifferenceSettings in alphabetical order.
fn difference_operands(
    vm: &mut Vm,
    a: &CallArgs,
    method: &str,
) -> JsResult<(seam::PlainDate, seam::PlainDate, RoundingOptions)> {
    let d = this_date(vm, a.this, method)?;
    let other = to_temporal_date(vm, a.arg(0), Value::Undefined)?;
    if d.calendar() != other.calendar() {
        return vm.range_error("the dates have different calendars");
    }
    let bag = get_options_object(vm, a.arg(1))?;
    let settings = RoundingOptions {
        largest_unit: get_unit(vm, bag, "largestUnit")?,
        increment: get_rounding_increment(vm, bag)?,
        rounding_mode: get_rounding_mode(vm, bag)?,
        smallest_unit: get_unit(vm, bag, "smallestUnit")?,
    };
    require_date_unit(vm, settings.largest_unit, "largestUnit", true)?;
    require_date_unit(vm, settings.smallest_unit, "smallestUnit", false)?;
    Ok((d, other, settings))
}

fn until(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (d, other, settings) = difference_operands(vm, a, "until")?;
    let r = d.until(&other, settings).or_throw(vm)?;
    wrap_duration(vm, r)
}

fn since(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (d, other, settings) = difference_operands(vm, a, "since")?;
    let r = d.since(&other, settings).or_throw(vm)?;
    wrap_duration(vm, r)
}

fn equals(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_date(vm, a.this, "equals")?;
    let other = to_temporal_date(vm, a.arg(0), Value::Undefined)?;
    Ok(Value::Bool(d.compare(&other).is_eq() && d.calendar() == other.calendar()))
}

fn to_string(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_date(vm, a.this, "toString")?;
    let bag = get_options_object(vm, a.arg(0))?;
    let show_calendar = get_show_calendar(vm, bag)?;
    let s = d.to_string_with(show_calendar).or_throw(vm)?;
    Ok(vm.str(&s))
}

fn to_json(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_date(vm, a.this, "toJSON")?;
    let s = d.to_string_with(Default::default()).or_throw(vm)?;
    Ok(vm.str(&s))
}

/// `toString()` until Intl formats Temporal objects (TASK-103.8.9).
fn to_locale_string(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_date(vm, a.this, "toLocaleString")?;
    let s = d.to_string_with(Default::default()).or_throw(vm)?;
    Ok(vm.str(&s))
}

fn value_of(vm: &mut Vm, _: &CallArgs) -> JsResult<Value> {
    vm.type_error("Temporal.PlainDate cannot be converted to a primitive; use Temporal.PlainDate.compare")
}
