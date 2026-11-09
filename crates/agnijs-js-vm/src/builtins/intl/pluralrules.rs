//! `Intl.PluralRules` (ECMA-402 §16).

use agnijs_js_intl::decimal::Dec;
use agnijs_js_intl::number::{Digits, Num};
use agnijs_js_intl::plural::PluralSelector;

use crate::object::{CallArgs, Class};
use crate::realm::Intrinsic;
use crate::value::{ObjId, Value};
use crate::vm::{JsResult, Vm};

use super::abstract_ops::*;
use super::numberformat::{put_digit_options, rounding_mode_name, set_digit_options};
use super::{create_intl_object, intl_data, put_str, put_value, string_list_to_array, IntlData};

pub struct PluralRulesRecord {
    pub locale: String,
    pub ordinal: bool,
    pub notation: String,
    /// Only set for compact notation.
    pub compact_display: Option<String>,
    pub digits: Digits,
    pub rounding_priority: String,
    pub selector: PluralSelector,
}

pub fn install(vm: &mut Vm, intl: ObjId) {
    let (_ctor, proto) = super::install_service(
        vm,
        intl,
        "PluralRules",
        0,
        constructor,
        Intrinsic::IntlPluralRules,
        Intrinsic::IntlPluralRulesPrototype,
        supported_locales_of,
    );
    vm.method(proto, "select", 1, select);
    vm.method(proto, "selectRange", 2, select_range);
    vm.method(proto, "resolvedOptions", 0, resolved_options);
}

fn constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if !a.is_construct() {
        return vm.type_error("Intl.PluralRules must be called with new");
    }
    let requested = canonicalize_locale_list(vm, a.arg(0))?;
    let options = coerce_options_to_object(vm, a.arg(1))?;
    get_locale_matcher(vm, options)?;
    let ty = get_string_option(vm, options, "type", &["cardinal", "ordinal"])?.unwrap_or_else(|| "cardinal".to_string());
    let notation = get_string_option(vm, options, "notation", &["standard", "scientific", "engineering", "compact"])?.unwrap_or_else(|| "standard".to_string());
    let compact_display = get_string_option(vm, options, "compactDisplay", &["short", "long"])?.unwrap_or_else(|| "short".to_string());
    let digit_opts = set_digit_options(vm, options, 0.0, 3.0, notation == "compact")?;
    let r = resolve_locale(&requested, &[], &[], &|_, _| Vec::new(), vm);
    let selector = match PluralSelector::new(&r.locale, ty == "ordinal", digit_opts.digits.clone(), &notation, compact_display == "long") {
        Ok(s) => s,
        Err(e) => return vm.range_error(&e),
    };
    let record = PluralRulesRecord {
        locale: r.locale,
        ordinal: ty == "ordinal",
        compact_display: if notation == "compact" { Some(compact_display) } else { None },
        notation,
        digits: digit_opts.digits,
        rounding_priority: digit_opts.rounding_priority,
        selector,
    };
    let obj = create_intl_object(vm, a, Intrinsic::IntlPluralRulesPrototype, IntlData::PluralRules(Box::new(record)))?;
    Ok(Value::Object(obj))
}

fn this_pr(vm: &mut Vm, this: Value, method: &str) -> JsResult<ObjId> {
    match intl_data(vm, this) {
        Some(IntlData::PluralRules(_)) => Ok(this.as_object().unwrap()),
        _ => vm.type_error(&format!("Intl.PluralRules.prototype.{method} called on an incompatible receiver")),
    }
}

fn record(vm: &Vm, o: ObjId) -> &PluralRulesRecord {
    match &vm.heap.obj(o).class {
        Class::Intl(d) => match &**d {
            IntlData::PluralRules(r) => r,
            _ => unreachable!(),
        },
        _ => unreachable!(),
    }
}

/// ResolvePlural: non-finite values are "other".
fn category(r: &PluralRulesRecord, n: f64) -> &'static str {
    if !n.is_finite() {
        return "other";
    }
    r.selector.select(&Dec::from_f64(n))
}

fn select(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let pr = this_pr(vm, a.this, "select")?;
    let n = vm.to_number(a.arg(0))?;
    let c = category(record(vm, pr), n);
    Ok(vm.str(c))
}

fn select_range(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let pr = this_pr(vm, a.this, "selectRange")?;
    if a.arg(0).is_undefined() || a.arg(1).is_undefined() {
        return vm.type_error("selectRange requires two values");
    }
    let x = vm.to_number(a.arg(0))?;
    let y = vm.to_number(a.arg(1))?;
    if x.is_nan() || y.is_nan() {
        return vm.range_error("selectRange does not accept NaN");
    }
    let r = record(vm, pr);
    let c = if !x.is_finite() || !y.is_finite() {
        // ResolvePluralRange formats both ends; infinities take the
        // category of the end that is finite, else "other".
        let _ = Num::from_f64(x);
        if x.is_finite() {
            category(r, x)
        } else if y.is_finite() {
            category(r, y)
        } else {
            "other"
        }
    } else {
        r.selector.select_range(&Dec::from_f64(x), &Dec::from_f64(y))
    };
    Ok(vm.str(c))
}

fn resolved_options(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let pr = this_pr(vm, a.this, "resolvedOptions")?;
    let o = vm.new_object();
    let (locale, ordinal, notation, compact_display, digits, priority, categories) = {
        let r = record(vm, pr);
        (r.locale.clone(), r.ordinal, r.notation.clone(), r.compact_display.clone(), r.digits.clone(), r.rounding_priority.clone(), r.selector.categories())
    };
    put_str(vm, o, "locale", &locale)?;
    put_str(vm, o, "type", if ordinal { "ordinal" } else { "cardinal" })?;
    put_str(vm, o, "notation", &notation)?;
    if let Some(cd) = &compact_display {
        put_str(vm, o, "compactDisplay", cd)?;
    }
    put_digit_options(vm, o, &digits, &priority, true)?;
    let cats: Vec<String> = categories.iter().map(|s| s.to_string()).collect();
    let arr = string_list_to_array(vm, &cats);
    put_value(vm, o, "pluralCategories", arr)?;
    put_value(vm, o, "roundingIncrement", Value::Number(digits.rounding_increment as f64))?;
    put_str(vm, o, "roundingMode", rounding_mode_name(digits.rounding_mode))?;
    put_str(vm, o, "roundingPriority", &priority)?;
    put_str(
        vm,
        o,
        "trailingZeroDisplay",
        if digits.trailing_zero_display == agnijs_js_intl::number::TrailingZeroDisplay::StripIfInteger { "stripIfInteger" } else { "auto" },
    )?;
    Ok(Value::Object(o))
}
