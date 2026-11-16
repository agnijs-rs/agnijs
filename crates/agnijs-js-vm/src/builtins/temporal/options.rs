//! Reading Temporal options bags (the abstract operations of the
//! specification's "Temporal options" section). Every reader performs one
//! `Get` and the coercion the specification names, so a bag with getters
//! observes the specification's order; a method reads its options in the
//! order its algorithm lists them (alphabetical, as a rule).

use agnijs_js_temporal::{Precision, RoundingMode, Unit};

use crate::value::{ObjId, Value};
use crate::vm::{JsResult, Vm};

/// GetOptionsObject: `undefined` is an empty bag (`None`), an object is
/// the bag, anything else is a `TypeError`.
pub fn get_options_object(vm: &mut Vm, options: Value) -> JsResult<Option<ObjId>> {
    match options {
        Value::Undefined => Ok(None),
        Value::Object(o) => Ok(Some(o)),
        _ => vm.type_error("options must be an object or undefined"),
    }
}

/// `Get(options, name)`, `undefined` for an empty bag.
pub fn get(vm: &mut Vm, options: Option<ObjId>, name: &str) -> JsResult<Value> {
    match options {
        Some(o) => vm.get_str(o, name),
        None => Ok(Value::Undefined),
    }
}

/// GetOption of type string: `None` when undefined, else ToString and one
/// of the values `parse` accepts, or a `RangeError`.
fn get_string_option<T>(
    vm: &mut Vm,
    options: Option<ObjId>,
    name: &str,
    parse: impl Fn(&str) -> Option<T>,
) -> JsResult<Option<T>> {
    let v = get(vm, options, name)?;
    if v.is_undefined() {
        return Ok(None);
    }
    let s = vm.to_rust_string(v)?;
    match parse(&s) {
        Some(t) => Ok(Some(t)),
        None => vm.range_error(&format!("{s} is not a valid value for {name}")),
    }
}

/// GetTemporalUnitValuedOption: a unit name, singular or plural, or
/// `"auto"`. Whether the unit suits the method (ValidateTemporalUnitValue)
/// is checked after every option is read.
pub fn get_unit(vm: &mut Vm, options: Option<ObjId>, name: &str) -> JsResult<Option<Unit>> {
    get_string_option(vm, options, name, Unit::parse)
}

/// GetRoundingModeOption, without its default.
pub fn get_rounding_mode(vm: &mut Vm, options: Option<ObjId>) -> JsResult<Option<RoundingMode>> {
    get_string_option(vm, options, "roundingMode", RoundingMode::parse)
}

/// GetRoundingIncrementOption: `None` when undefined, else
/// ToIntegerWithTruncation in 1..=10⁹.
pub fn get_rounding_increment(vm: &mut Vm, options: Option<ObjId>) -> JsResult<Option<u32>> {
    let v = get(vm, options, "roundingIncrement")?;
    if v.is_undefined() {
        return Ok(None);
    }
    let n = to_integer_with_truncation(vm, v)?;
    if !(1.0..=1e9).contains(&n) {
        return vm.range_error("roundingIncrement must be an integer from 1 to 1e9");
    }
    Ok(Some(n as u32))
}

/// GetTemporalFractionalSecondDigitsOption.
pub fn get_fractional_second_digits(vm: &mut Vm, options: Option<ObjId>) -> JsResult<Precision> {
    let v = get(vm, options, "fractionalSecondDigits")?;
    let n = match v {
        Value::Undefined => return Ok(Precision::Auto),
        Value::Number(n) => n,
        _ => {
            let s = vm.to_rust_string(v)?;
            if s == "auto" {
                return Ok(Precision::Auto);
            }
            return vm.range_error(&format!("{s} is not a valid value for fractionalSecondDigits"));
        }
    };
    if !n.is_finite() {
        return vm.range_error("fractionalSecondDigits must be a finite number");
    }
    let digits = n.floor();
    if !(0.0..=9.0).contains(&digits) {
        return vm.range_error("fractionalSecondDigits must be from 0 to 9");
    }
    Ok(Precision::Digits(digits as u8))
}

/// ToIntegerWithTruncation: ToNumber, a `RangeError` for NaN and the
/// infinities, then truncation.
pub fn to_integer_with_truncation(vm: &mut Vm, v: Value) -> JsResult<f64> {
    let n = vm.to_number(v)?;
    if !n.is_finite() {
        return vm.range_error("expected a finite number");
    }
    Ok(n.trunc() + 0.0)
}

/// ToIntegerIfIntegral: ToNumber, then a `RangeError` unless the number is
/// an integer.
pub fn to_integer_if_integral(vm: &mut Vm, v: Value) -> JsResult<f64> {
    let n = vm.to_number(v)?;
    if !n.is_finite() || n.trunc() != n {
        return vm.range_error("expected an integer");
    }
    Ok(n + 0.0)
}
