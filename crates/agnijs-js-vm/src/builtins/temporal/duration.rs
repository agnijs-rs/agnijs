//! `Temporal.Duration`.

use agnijs_js_temporal::{self as seam, DurationFields, RoundingOptions, ToStringOptions, Unit};

use super::options::{
    get, get_fractional_second_digits, get_options_object, get_rounding_increment, get_rounding_mode, get_unit,
    to_integer_if_integral,
};
use super::{create, install_class, temporal_data, OrThrow, TemporalData};
use crate::object::CallArgs;
use crate::realm::Intrinsic;
use crate::value::{ObjId, Value};
use crate::vm::{JsResult, Vm};

/// The fields in the order the constructor takes them.
const FIELD_NAMES: [&str; 10] = [
    "years",
    "months",
    "weeks",
    "days",
    "hours",
    "minutes",
    "seconds",
    "milliseconds",
    "microseconds",
    "nanoseconds",
];

/// The constructor's field indices in the alphabetical order
/// ToTemporalPartialDurationRecord reads a property bag in.
const ALPHABETICAL: [usize; 10] = [3, 4, 8, 7, 5, 1, 9, 6, 2, 0];

pub fn install(vm: &mut Vm, namespace: ObjId) {
    let proto = vm.new_object();
    let ctor = install_class(
        vm,
        namespace,
        "Duration",
        0,
        construct,
        proto,
        Intrinsic::TemporalDuration,
        Intrinsic::TemporalDurationPrototype,
    );
    vm.method(ctor, "from", 1, from);
    vm.method(ctor, "compare", 2, compare);

    vm.accessor(proto, "years", get_years, None);
    vm.accessor(proto, "months", get_months, None);
    vm.accessor(proto, "weeks", get_weeks, None);
    vm.accessor(proto, "days", get_days, None);
    vm.accessor(proto, "hours", get_hours, None);
    vm.accessor(proto, "minutes", get_minutes, None);
    vm.accessor(proto, "seconds", get_seconds, None);
    vm.accessor(proto, "milliseconds", get_milliseconds, None);
    vm.accessor(proto, "microseconds", get_microseconds, None);
    vm.accessor(proto, "nanoseconds", get_nanoseconds, None);
    vm.accessor(proto, "sign", get_sign, None);
    vm.accessor(proto, "blank", get_blank, None);
    vm.method(proto, "with", 1, with);
    vm.method(proto, "negated", 0, negated);
    vm.method(proto, "abs", 0, abs);
    vm.method(proto, "add", 1, add);
    vm.method(proto, "subtract", 1, subtract);
    vm.method(proto, "round", 1, round);
    vm.method(proto, "total", 1, total);
    vm.method(proto, "toString", 0, to_string);
    vm.method(proto, "toJSON", 0, to_json);
    vm.method(proto, "toLocaleString", 0, to_locale_string);
    vm.method(proto, "valueOf", 0, value_of);
    super::super::to_string_tag(vm, proto, "Temporal.Duration");
}

/// RequireInternalSlot(this, [[InitializedTemporalDuration]]).
fn this_duration(vm: &mut Vm, this: Value, method: &str) -> JsResult<seam::Duration> {
    match temporal_data(vm, this) {
        Some(TemporalData::Duration(d)) => Ok(d.clone()),
        _ => vm.type_error(&format!("Temporal.Duration.prototype.{method} called on an incompatible receiver")),
    }
}

/// CreateTemporalDuration with the fields as numbers: a `RangeError`
/// unless they make a valid duration. Every integral number beyond
/// `i64` (or `i128`) is beyond IsValidDuration's limits too.
fn fields_from_numbers(vm: &mut Vm, n: [f64; 10]) -> JsResult<DurationFields> {
    // 2⁶³ and 2¹²⁷, exactly.
    const I64: f64 = i64::MAX as f64;
    const I128: f64 = i128::MAX as f64;
    let limits = [I64, I64, I64, I64, I64, I64, I64, I64, I128, I128];
    if n.iter().zip(limits).any(|(v, limit)| v.abs() >= limit) {
        return vm.range_error("the duration is out of range");
    }
    Ok(DurationFields {
        years: n[0] as i64,
        months: n[1] as i64,
        weeks: n[2] as i64,
        days: n[3] as i64,
        hours: n[4] as i64,
        minutes: n[5] as i64,
        seconds: n[6] as i64,
        milliseconds: n[7] as i64,
        microseconds: n[8] as i128,
        nanoseconds: n[9] as i128,
    })
}

fn numbers_from_fields(f: DurationFields) -> [f64; 10] {
    [
        f.years as f64,
        f.months as f64,
        f.weeks as f64,
        f.days as f64,
        f.hours as f64,
        f.minutes as f64,
        f.seconds as f64,
        f.milliseconds as f64,
        f.microseconds as f64,
        f.nanoseconds as f64,
    ]
}

fn new_duration(vm: &mut Vm, n: [f64; 10]) -> JsResult<seam::Duration> {
    let fields = fields_from_numbers(vm, n)?;
    seam::Duration::new(fields).or_throw(vm)
}

fn wrap(vm: &mut Vm, d: seam::Duration) -> JsResult<Value> {
    let o = create(vm, Value::Undefined, Intrinsic::TemporalDurationPrototype, TemporalData::Duration(d))?;
    Ok(Value::Object(o))
}

/// `new Temporal.Duration(years, ..., nanoseconds)`.
fn construct(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if a.new_target.is_undefined() {
        return vm.type_error("Temporal.Duration must be called with new");
    }
    let mut n = [0.0; 10];
    for (i, slot) in n.iter_mut().enumerate() {
        let v = a.arg(i);
        if !v.is_undefined() {
            *slot = to_integer_if_integral(vm, v)?;
        }
    }
    let d = new_duration(vm, n)?;
    let o = create(vm, a.new_target, Intrinsic::TemporalDurationPrototype, TemporalData::Duration(d))?;
    Ok(Value::Object(o))
}

/// ToTemporalPartialDurationRecord: the fields the bag defines, read in
/// alphabetical order; a `TypeError` when it is not an object or defines
/// none of them.
fn partial_duration(vm: &mut Vm, like: Value) -> JsResult<[Option<f64>; 10]> {
    let Value::Object(o) = like else {
        return vm.type_error("a duration-like object is required");
    };
    let mut partial = [None; 10];
    for i in ALPHABETICAL {
        let v = vm.get_str(o, FIELD_NAMES[i])?;
        if !v.is_undefined() {
            partial[i] = Some(to_integer_if_integral(vm, v)?);
        }
    }
    if partial.iter().all(Option::is_none) {
        return vm.type_error("a duration-like object needs at least one duration property");
    }
    Ok(partial)
}

/// ToTemporalDuration.
pub fn to_temporal_duration(vm: &mut Vm, item: Value) -> JsResult<seam::Duration> {
    if let Some(TemporalData::Duration(d)) = temporal_data(vm, item) {
        return Ok(d.clone());
    }
    match item {
        Value::Object(_) => {
            let partial = partial_duration(vm, item)?;
            new_duration(vm, partial.map(|v| v.unwrap_or(0.0)))
        }
        Value::String(_) => {
            let s = vm.to_rust_string(item)?;
            seam::Duration::parse(&s).or_throw(vm)
        }
        _ => vm.type_error("a Temporal.Duration, a duration string or a duration-like object is required"),
    }
}

/// GetTemporalRelativeToOption.
fn get_relative_to(vm: &mut Vm, options: Option<ObjId>) -> JsResult<Option<seam::RelativeTo>> {
    let v = get(vm, options, "relativeTo")?;
    match v {
        Value::Undefined => Ok(None),
        Value::String(_) => {
            let s = vm.to_rust_string(v)?;
            seam::RelativeTo::parse(&s).or_throw(vm).map(Some)
        }
        // Temporal objects and property bags as relativeTo arrive with
        // Temporal.PlainDate and Temporal.ZonedDateTime (TASK-103.8.4,
        // TASK-103.8.8).
        Value::Object(_) => vm.type_error("relativeTo objects are not supported yet"),
        _ => vm.type_error("relativeTo must be a string or an object"),
    }
}

fn from(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = to_temporal_duration(vm, a.arg(0))?;
    wrap(vm, d)
}

fn compare(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let one = to_temporal_duration(vm, a.arg(0))?;
    let two = to_temporal_duration(vm, a.arg(1))?;
    let options = get_options_object(vm, a.arg(2))?;
    let relative_to = get_relative_to(vm, options)?;
    let order = one.compare(&two, relative_to.as_ref()).or_throw(vm)?;
    Ok(Value::Number(order as i8 as f64))
}

fn field(vm: &mut Vm, a: &CallArgs, i: usize) -> JsResult<Value> {
    let d = this_duration(vm, a.this, FIELD_NAMES[i])?;
    Ok(Value::Number(numbers_from_fields(d.fields())[i] + 0.0))
}

fn get_years(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    field(vm, a, 0)
}
fn get_months(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    field(vm, a, 1)
}
fn get_weeks(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    field(vm, a, 2)
}
fn get_days(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    field(vm, a, 3)
}
fn get_hours(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    field(vm, a, 4)
}
fn get_minutes(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    field(vm, a, 5)
}
fn get_seconds(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    field(vm, a, 6)
}
fn get_milliseconds(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    field(vm, a, 7)
}
fn get_microseconds(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    field(vm, a, 8)
}
fn get_nanoseconds(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    field(vm, a, 9)
}

fn get_sign(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_duration(vm, a.this, "sign")?;
    Ok(Value::Number(f64::from(d.sign())))
}

fn get_blank(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_duration(vm, a.this, "blank")?;
    Ok(Value::Bool(d.sign() == 0))
}

fn with(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_duration(vm, a.this, "with")?;
    let partial = partial_duration(vm, a.arg(0))?;
    let current = numbers_from_fields(d.fields());
    let mut n = [0.0; 10];
    for i in 0..10 {
        n[i] = partial[i].unwrap_or(current[i]);
    }
    let d = new_duration(vm, n)?;
    wrap(vm, d)
}

fn negated(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_duration(vm, a.this, "negated")?;
    wrap(vm, d.negated())
}

fn abs(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_duration(vm, a.this, "abs")?;
    wrap(vm, d.abs())
}

fn add(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_duration(vm, a.this, "add")?;
    let other = to_temporal_duration(vm, a.arg(0))?;
    let sum = d.add(&other).or_throw(vm)?;
    wrap(vm, sum)
}

fn subtract(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_duration(vm, a.this, "subtract")?;
    let other = to_temporal_duration(vm, a.arg(0))?;
    let difference = d.subtract(&other).or_throw(vm)?;
    wrap(vm, difference)
}

/// A string argument in place of an options bag (`round("hour")`,
/// `total("day")`): the value of the one option it stands for.
fn unit_from_string(vm: &mut Vm, v: Value, name: &str) -> JsResult<Unit> {
    let s = vm.to_rust_string(v)?;
    match Unit::parse(&s) {
        Some(u) => Ok(u),
        None => vm.range_error(&format!("{s} is not a valid value for {name}")),
    }
}

fn round(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_duration(vm, a.this, "round")?;
    let round_to = a.arg(0);
    let mut options = RoundingOptions::default();
    let mut relative_to = None;
    match round_to {
        Value::Undefined => return vm.type_error("Temporal.Duration.prototype.round requires options"),
        Value::String(_) => options.smallest_unit = Some(unit_from_string(vm, round_to, "smallestUnit")?),
        _ => {
            let bag = get_options_object(vm, round_to)?;
            options.largest_unit = get_unit(vm, bag, "largestUnit")?;
            relative_to = get_relative_to(vm, bag)?;
            options.increment = get_rounding_increment(vm, bag)?;
            options.rounding_mode = get_rounding_mode(vm, bag)?;
            options.smallest_unit = get_unit(vm, bag, "smallestUnit")?;
        }
    }
    if options.smallest_unit == Some(Unit::Auto) {
        return vm.range_error("smallestUnit cannot be auto");
    }
    if options.largest_unit.is_none() && options.smallest_unit.is_none() {
        return vm.range_error("one of smallestUnit or largestUnit is required");
    }
    let rounded = d.round(options, relative_to.as_ref()).or_throw(vm)?;
    wrap(vm, rounded)
}

fn total(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_duration(vm, a.this, "total")?;
    let total_of = a.arg(0);
    let (relative_to, unit) = match total_of {
        Value::Undefined => return vm.type_error("Temporal.Duration.prototype.total requires a unit"),
        Value::String(_) => (None, Some(unit_from_string(vm, total_of, "unit")?)),
        _ => {
            let bag = get_options_object(vm, total_of)?;
            let relative_to = get_relative_to(vm, bag)?;
            (relative_to, get_unit(vm, bag, "unit")?)
        }
    };
    let unit = match unit {
        None => return vm.range_error("unit is required"),
        Some(Unit::Auto) => return vm.range_error("unit cannot be auto"),
        Some(u) => u,
    };
    let total = d.total(unit, relative_to.as_ref()).or_throw(vm)?;
    Ok(Value::Number(total))
}

fn to_string(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_duration(vm, a.this, "toString")?;
    let bag = get_options_object(vm, a.arg(0))?;
    let options = ToStringOptions {
        precision: get_fractional_second_digits(vm, bag)?,
        rounding_mode: get_rounding_mode(vm, bag)?,
        smallest_unit: get_unit(vm, bag, "smallestUnit")?,
    };
    let s = d.to_string_with(options).or_throw(vm)?;
    Ok(vm.str(&s))
}

fn to_json(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_duration(vm, a.this, "toJSON")?;
    let s = d.to_string_with(ToStringOptions::default()).or_throw(vm)?;
    Ok(vm.str(&s))
}

/// Without Intl.DurationFormat (a later proposal) this is `toString()`.
fn to_locale_string(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let d = this_duration(vm, a.this, "toLocaleString")?;
    let s = d.to_string_with(ToStringOptions::default()).or_throw(vm)?;
    Ok(vm.str(&s))
}

fn value_of(vm: &mut Vm, _: &CallArgs) -> JsResult<Value> {
    vm.type_error("Temporal.Duration cannot be converted to a primitive; use Temporal.Duration.compare")
}
