//! `Intl.NumberFormat` (ECMA-402 §15, including the NumberFormat v3
//! additions that are part of the 2023 edition onwards).

use gable_js_intl::decimal::RoundingMode;
use gable_js_intl::number::{
    CurrencyDisplay, CurrencySign, Digits, Grouping, Notation, Num, NumberFormatOptions, NumberFormatter, Part, RangePart, RoundingType,
    SignDisplay, Style, TrailingZeroDisplay, UnitDisplay,
};

use crate::object::{CallArgs, Class};
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, Value};
use crate::vm::{JsResult, Vm};

use super::abstract_ops::*;
use super::{create_intl_object, intl_data, put_str, put_value, IntlData};

pub struct NumberFormatRecord {
    pub formatter: NumberFormatter,
    pub locale: String,
    pub use_grouping: String,
    pub rounding_priority: String,
    pub compact_display: Option<String>,
    pub bound_format: Option<ObjId>,
}

pub fn install(vm: &mut Vm, intl: ObjId) {
    let (_ctor, proto) = super::install_service(
        vm,
        intl,
        "NumberFormat",
        0,
        constructor,
        Intrinsic::IntlNumberFormat,
        Intrinsic::IntlNumberFormatPrototype,
        supported_locales_of,
    );
    vm.accessor(proto, "format", format_getter, None);
    vm.method(proto, "formatToParts", 1, format_to_parts);
    vm.method(proto, "formatRange", 2, format_range);
    vm.method(proto, "formatRangeToParts", 2, format_range_to_parts);
    vm.method(proto, "resolvedOptions", 0, resolved_options);
}

/// Digit options shared with `Intl.PluralRules`.
pub struct DigitOptions {
    pub digits: Digits,
    pub rounding_priority: String,
}

/// SetNumberFormatDigitOptions(intlObj, options, mnfdDefault, mxfdDefault, notation)
pub fn set_digit_options(vm: &mut Vm, options: ObjId, mnfd_default: f64, mut mxfd_default: f64, compact: bool) -> JsResult<DigitOptions> {
    let mnid = get_number_option(vm, options, "minimumIntegerDigits", 1.0, 21.0, Some(1.0))?.unwrap();
    let mnfd_v = vm.get_str(options, "minimumFractionDigits")?;
    let mxfd_v = vm.get_str(options, "maximumFractionDigits")?;
    let mnsd_v = vm.get_str(options, "minimumSignificantDigits")?;
    let mxsd_v = vm.get_str(options, "maximumSignificantDigits")?;
    let increment = get_number_option(vm, options, "roundingIncrement", 1.0, 5000.0, Some(1.0))?.unwrap();
    if ![1.0, 2.0, 5.0, 10.0, 20.0, 25.0, 50.0, 100.0, 200.0, 250.0, 500.0, 1000.0, 2000.0, 2500.0, 5000.0].contains(&increment) {
        return vm.range_error(&format!("Invalid roundingIncrement {increment}"));
    }
    let mode = get_string_option(
        vm,
        options,
        "roundingMode",
        &["ceil", "floor", "expand", "trunc", "halfCeil", "halfFloor", "halfExpand", "halfTrunc", "halfEven"],
    )?
    .unwrap_or_else(|| "halfExpand".to_string());
    let priority = get_string_option(vm, options, "roundingPriority", &["auto", "morePrecision", "lessPrecision"])?.unwrap_or_else(|| "auto".to_string());
    let tzd = get_string_option(vm, options, "trailingZeroDisplay", &["auto", "stripIfInteger"])?.unwrap_or_else(|| "auto".to_string());
    if increment != 1.0 {
        mxfd_default = mnfd_default;
    }
    let has_sd = !mnsd_v.is_undefined() || !mxsd_v.is_undefined();
    let has_fd = !mnfd_v.is_undefined() || !mxfd_v.is_undefined();
    let mut need_sd = true;
    let mut need_fd = true;
    if priority == "auto" {
        need_sd = has_sd;
        if need_sd || (!has_fd && compact) {
            need_fd = false;
        }
    }
    let (mut mnsd, mut mxsd) = (1.0, 21.0);
    if need_sd {
        if has_sd {
            mnsd = default_number_option(vm, mnsd_v, 1.0, 21.0, Some(1.0))?.unwrap();
            mxsd = default_number_option(vm, mxsd_v, mnsd, 21.0, Some(21.0))?.unwrap();
        }
    }
    let (mut mnfd, mut mxfd) = (mnfd_default, mxfd_default);
    if need_fd {
        if has_fd {
            let a = default_number_option(vm, mnfd_v, 0.0, 100.0, None)?;
            let b = default_number_option(vm, mxfd_v, 0.0, 100.0, None)?;
            match (a, b) {
                (None, Some(b)) => {
                    mnfd = mnfd_default.min(b);
                    mxfd = b;
                }
                (Some(a), None) => {
                    mnfd = a;
                    mxfd = mxfd_default.max(a);
                }
                (Some(a), Some(b)) => {
                    if a > b {
                        return vm.range_error("minimumFractionDigits is greater than maximumFractionDigits");
                    }
                    mnfd = a;
                    mxfd = b;
                }
                (None, None) => unreachable!(),
            }
        }
    }
    let rounding_type;
    let mut computed_priority = priority.clone();
    if !need_sd && !need_fd {
        mnfd = 0.0;
        mxfd = 0.0;
        mnsd = 1.0;
        mxsd = 2.0;
        rounding_type = RoundingType::MorePrecision;
        computed_priority = "morePrecision".to_string();
    } else if priority == "morePrecision" {
        rounding_type = RoundingType::MorePrecision;
    } else if priority == "lessPrecision" {
        rounding_type = RoundingType::LessPrecision;
    } else if has_sd {
        rounding_type = RoundingType::SignificantDigits;
    } else {
        rounding_type = RoundingType::FractionDigits;
    }
    if increment != 1.0 {
        if rounding_type != RoundingType::FractionDigits {
            return vm.type_error("roundingIncrement requires fraction-digit rounding");
        }
        if mxfd != mnfd {
            return vm.range_error("roundingIncrement requires maximumFractionDigits to equal minimumFractionDigits");
        }
    }
    Ok(DigitOptions {
        digits: Digits {
            min_integer: mnid as usize,
            min_fraction: mnfd as usize,
            max_fraction: mxfd as usize,
            min_significant: mnsd as usize,
            max_significant: mxsd as usize,
            rounding_type,
            rounding_mode: RoundingMode::parse(&mode).unwrap_or(RoundingMode::HalfExpand),
            rounding_increment: increment as u32,
            trailing_zero_display: if tzd == "stripIfInteger" { TrailingZeroDisplay::StripIfInteger } else { TrailingZeroDisplay::Auto },
        },
        rounding_priority: computed_priority,
    })
}

/// The `nu` locale data: the locale's default first, then every simple
/// numbering system.
pub fn numbering_system_data(locale: &str) -> Vec<String> {
    let mut v = gable_js_intl::data::numbering_systems_for(locale);
    for nu in gable_js_intl::data::numbering_systems() {
        if !v.contains(&nu) {
            v.push(nu);
        }
    }
    v
}

/// Intl.NumberFormat([locales [, options]])
fn constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let new_target = if a.new_target.is_undefined() { Value::Object(a.callee) } else { a.new_target };
    let requested = canonicalize_locale_list(vm, a.arg(0))?;
    let options = coerce_options_to_object(vm, a.arg(1))?;
    get_locale_matcher(vm, options)?;
    let nu_opt = get_string_option(vm, options, "numberingSystem", &[])?;
    if let Some(nu) = &nu_opt {
        if !is_unicode_type(nu) {
            return vm.range_error(&format!("Invalid numberingSystem {nu}"));
        }
    }
    let r = resolve_locale(&requested, &["nu"], &[nu_opt], &|loc, _| numbering_system_data(loc), vm);
    let numbering_system = r.keywords[0].clone().unwrap_or_else(|| "latn".to_string());

    // SetNumberFormatUnitOptions
    let style = get_string_option(vm, options, "style", &["decimal", "percent", "currency", "unit"])?.unwrap_or_else(|| "decimal".to_string());
    let currency = get_string_option(vm, options, "currency", &[])?;
    if currency.is_none() && style == "currency" {
        return vm.type_error("Currency code is required with currency style");
    }
    if let Some(c) = &currency {
        if !gable_js_intl::data::is_well_formed_currency(c) {
            return vm.range_error(&format!("Invalid currency code {c}"));
        }
    }
    let currency_display = get_string_option(vm, options, "currencyDisplay", &["code", "symbol", "narrowSymbol", "name"])?.unwrap_or_else(|| "symbol".to_string());
    let currency_sign = get_string_option(vm, options, "currencySign", &["standard", "accounting"])?.unwrap_or_else(|| "standard".to_string());
    let unit = get_string_option(vm, options, "unit", &[])?;
    if unit.is_none() && style == "unit" {
        return vm.type_error("Unit is required with unit style");
    }
    if let Some(u) = &unit {
        if !gable_js_intl::data::is_well_formed_unit(u) {
            return vm.range_error(&format!("Invalid unit {u}"));
        }
    }
    let unit_display = get_string_option(vm, options, "unitDisplay", &["short", "narrow", "long"])?.unwrap_or_else(|| "short".to_string());
    let seam_style = match style.as_str() {
        "currency" => {
            let Some(code) = currency else { unreachable!() };
            Style::Currency {
                code: code.to_ascii_uppercase(),
                display: match currency_display.as_str() {
                    "code" => CurrencyDisplay::Code,
                    "narrowSymbol" => CurrencyDisplay::NarrowSymbol,
                    "name" => CurrencyDisplay::Name,
                    _ => CurrencyDisplay::Symbol,
                },
                sign: if currency_sign == "accounting" { CurrencySign::Accounting } else { CurrencySign::Standard },
            }
        }
        "unit" => {
            let Some(u) = unit else { unreachable!() };
            Style::Unit {
                unit: u,
                display: match unit_display.as_str() {
                    "narrow" => UnitDisplay::Narrow,
                    "long" => UnitDisplay::Long,
                    _ => UnitDisplay::Short,
                },
            }
        }
        "percent" => Style::Percent,
        _ => Style::Decimal,
    };
    let notation = get_string_option(vm, options, "notation", &["standard", "scientific", "engineering", "compact"])?.unwrap_or_else(|| "standard".to_string());
    // Currency digits are the default only in standard notation.
    let (mnfd_default, mxfd_default) = match &seam_style {
        Style::Currency { code, .. } if notation == "standard" => {
            let d = gable_js_intl::number::currency_digits(code) as f64;
            (d, d)
        }
        Style::Percent => (0.0, 0.0),
        _ => (0.0, 3.0),
    };
    let digit_opts = set_digit_options(vm, options, mnfd_default, mxfd_default, notation == "compact")?;
    let compact_display = get_string_option(vm, options, "compactDisplay", &["short", "long"])?.unwrap_or_else(|| "short".to_string());
    let default_grouping = if notation == "compact" { "min2" } else { "auto" };
    let grouping = get_bool_or_string_option(vm, options, "useGrouping", &["min2", "auto", "always"], BoolOrString::Str(default_grouping.to_string()))?;
    let use_grouping = match grouping {
        BoolOrString::Bool(true) => "always".to_string(),
        BoolOrString::Bool(false) => "false".to_string(),
        BoolOrString::Str(s) => s,
    };
    let sign_display = get_string_option(vm, options, "signDisplay", &["auto", "never", "always", "exceptZero", "negative"])?.unwrap_or_else(|| "auto".to_string());

    let seam_opts = NumberFormatOptions {
        locale: r.locale.clone(),
        numbering_system: numbering_system.clone(),
        style: seam_style,
        notation: match notation.as_str() {
            "scientific" => Notation::Scientific,
            "engineering" => Notation::Engineering,
            "compact" => Notation::Compact { long: compact_display == "long" },
            _ => Notation::Standard,
        },
        grouping: match use_grouping.as_str() {
            "always" => Grouping::Always,
            "min2" => Grouping::Min2,
            "false" => Grouping::Never,
            _ => Grouping::Auto,
        },
        sign_display: match sign_display.as_str() {
            "never" => SignDisplay::Never,
            "always" => SignDisplay::Always,
            "exceptZero" => SignDisplay::ExceptZero,
            "negative" => SignDisplay::Negative,
            _ => SignDisplay::Auto,
        },
        digits: digit_opts.digits,
    };
    let formatter = match NumberFormatter::new(seam_opts) {
        Ok(f) => f,
        Err(e) => return vm.range_error(&e),
    };
    let record = NumberFormatRecord {
        formatter,
        locale: r.locale,
        use_grouping,
        rounding_priority: digit_opts.rounding_priority,
        compact_display: if notation == "compact" { Some(compact_display) } else { None },
        bound_format: None,
    };
    let args = CallArgs { this: a.this, args: a.args, new_target, callee: a.callee };
    let obj = create_intl_object(vm, &args, Intrinsic::IntlNumberFormatPrototype, IntlData::NumberFormat(Box::new(record)))?;
    // Legacy ChainNumberFormat: `Intl.NumberFormat.call(nfInstance)`.
    if a.new_target.is_undefined() {
        if let Value::Object(this) = a.this {
            let ctor = vm.intrinsic(Intrinsic::IntlNumberFormat);
            if vm.ordinary_has_instance(Value::Object(ctor), a.this)? {
                let key = crate::value::PropKey::Sym(vm.realm().intl_fallback);
                vm.define_property_or_throw(
                    this,
                    key,
                    crate::value::PropertyDescriptor::data(Value::Object(obj), Attrs::NONE),
                )?;
                return Ok(a.this);
            }
        }
    }
    Ok(Value::Object(obj))
}

/// UnwrapNumberFormat(nf): the receiver, or the legacy fallback slot.
fn unwrap(vm: &mut Vm, this: Value, method: &str) -> JsResult<ObjId> {
    if let Value::Object(o) = this {
        if matches!(&vm.heap.obj(o).class, Class::Intl(d) if matches!(**d, IntlData::NumberFormat(_))) {
            return Ok(o);
        }
        let ctor = vm.intrinsic(Intrinsic::IntlNumberFormat);
        if vm.ordinary_has_instance(Value::Object(ctor), this)? {
            let key = crate::value::PropKey::Sym(vm.realm().intl_fallback);
            let v = vm.get(o, key)?;
            if let Value::Object(inner) = v {
                if matches!(&vm.heap.obj(inner).class, Class::Intl(d) if matches!(**d, IntlData::NumberFormat(_))) {
                    return Ok(inner);
                }
            }
        }
    }
    vm.type_error(&format!("Intl.NumberFormat.prototype.{method} called on an incompatible receiver"))
}

fn record(vm: &Vm, o: ObjId) -> &NumberFormatRecord {
    match &vm.heap.obj(o).class {
        Class::Intl(d) => match &**d {
            IntlData::NumberFormat(r) => r,
            _ => unreachable!(),
        },
        _ => unreachable!(),
    }
}

fn format_getter(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = unwrap(vm, a.this, "format")?;
    if let Some(f) = record(vm, o).bound_format {
        return Ok(Value::Object(f));
    }
    let f = vm.new_native("", 1, bound_format);
    if let Class::Native(n) = &mut vm.heap.obj_mut(f).class {
        n.slots.push(Value::Object(o));
    }
    if let Class::Intl(d) = &mut vm.heap.obj_mut(o).class {
        if let IntlData::NumberFormat(r) = &mut **d {
            r.bound_format = Some(f);
        }
    }
    Ok(Value::Object(f))
}

fn bound_format(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let nf = match &vm.heap.obj(a.callee).class {
        Class::Native(n) => n.slots[0].as_object().unwrap(),
        _ => unreachable!(),
    };
    let x = to_intl_mathematical_value(vm, a.arg(0))?;
    let s = record(vm, nf).formatter.format(&x);
    Ok(vm.str(&s))
}

/// Parts → array of `{type, value}` (plus `source` for ranges, `unit` when given).
pub fn parts_to_array(vm: &mut Vm, parts: &[Part], unit: Option<&str>) -> JsResult<Value> {
    let mut items = Vec::with_capacity(parts.len());
    for p in parts {
        let o = vm.new_object();
        put_str(vm, o, "type", p.kind)?;
        put_str(vm, o, "value", &p.value)?;
        if let Some(u) = unit {
            put_str(vm, o, "unit", u)?;
        }
        items.push(Value::Object(o));
    }
    Ok(Value::Object(vm.new_array(items)))
}

pub fn range_parts_to_array(vm: &mut Vm, parts: &[RangePart]) -> JsResult<Value> {
    let mut items = Vec::with_capacity(parts.len());
    for p in parts {
        let o = vm.new_object();
        put_str(vm, o, "type", p.kind)?;
        put_str(vm, o, "value", &p.value)?;
        put_str(vm, o, "source", p.source)?;
        items.push(Value::Object(o));
    }
    Ok(Value::Object(vm.new_array(items)))
}

fn this_nf(vm: &mut Vm, this: Value, method: &str) -> JsResult<ObjId> {
    match intl_data(vm, this) {
        Some(IntlData::NumberFormat(_)) => Ok(this.as_object().unwrap()),
        _ => vm.type_error(&format!("Intl.NumberFormat.prototype.{method} called on an incompatible receiver")),
    }
}

fn format_to_parts(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let nf = this_nf(vm, a.this, "formatToParts")?;
    let x = to_intl_mathematical_value(vm, a.arg(0))?;
    let parts = record(vm, nf).formatter.format_to_parts(&x);
    parts_to_array(vm, &parts, None)
}

fn range_inputs(vm: &mut Vm, a: &CallArgs) -> JsResult<(Num, Num)> {
    if a.arg(0).is_undefined() || a.arg(1).is_undefined() {
        return vm.type_error("formatRange requires two values");
    }
    let x = to_intl_mathematical_value(vm, a.arg(0))?;
    let y = to_intl_mathematical_value(vm, a.arg(1))?;
    if matches!(x, Num::Nan) || matches!(y, Num::Nan) {
        return vm.range_error("formatRange does not accept NaN");
    }
    Ok((x, y))
}

fn format_range(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let nf = this_nf(vm, a.this, "formatRange")?;
    let (x, y) = range_inputs(vm, a)?;
    let s = record(vm, nf).formatter.format_range(&x, &y);
    Ok(vm.str(&s))
}

fn format_range_to_parts(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let nf = this_nf(vm, a.this, "formatRangeToParts")?;
    let (x, y) = range_inputs(vm, a)?;
    let parts = record(vm, nf).formatter.format_range_to_parts(&x, &y);
    range_parts_to_array(vm, &parts)
}

pub fn rounding_mode_name(m: RoundingMode) -> &'static str {
    match m {
        RoundingMode::Ceil => "ceil",
        RoundingMode::Floor => "floor",
        RoundingMode::Expand => "expand",
        RoundingMode::Trunc => "trunc",
        RoundingMode::HalfCeil => "halfCeil",
        RoundingMode::HalfFloor => "halfFloor",
        RoundingMode::HalfExpand => "halfExpand",
        RoundingMode::HalfTrunc => "halfTrunc",
        RoundingMode::HalfEven => "halfEven",
    }
}

/// The digit-related resolvedOptions entries, shared with PluralRules.
pub fn put_digit_options(vm: &mut Vm, o: ObjId, d: &Digits, rounding_priority: &str, with_increment: bool) -> JsResult<()> {
    put_value(vm, o, "minimumIntegerDigits", Value::Number(d.min_integer as f64))?;
    let fraction = matches!(d.rounding_type, RoundingType::FractionDigits | RoundingType::MorePrecision | RoundingType::LessPrecision);
    let significant = matches!(d.rounding_type, RoundingType::SignificantDigits | RoundingType::MorePrecision | RoundingType::LessPrecision);
    if fraction {
        put_value(vm, o, "minimumFractionDigits", Value::Number(d.min_fraction as f64))?;
        put_value(vm, o, "maximumFractionDigits", Value::Number(d.max_fraction as f64))?;
    }
    if significant {
        put_value(vm, o, "minimumSignificantDigits", Value::Number(d.min_significant as f64))?;
        put_value(vm, o, "maximumSignificantDigits", Value::Number(d.max_significant as f64))?;
    }
    let _ = (with_increment, rounding_priority);
    Ok(())
}

fn resolved_options(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let nf = unwrap(vm, a.this, "resolvedOptions")?;
    let o = vm.new_object();
    let (locale, use_grouping, priority, compact_display) = {
        let r = record(vm, nf);
        (r.locale.clone(), r.use_grouping.clone(), r.rounding_priority.clone(), r.compact_display.clone())
    };
    let opts = record(vm, nf).formatter.options().clone();
    put_str(vm, o, "locale", &locale)?;
    put_str(vm, o, "numberingSystem", &opts.numbering_system)?;
    match &opts.style {
        Style::Decimal => put_str(vm, o, "style", "decimal")?,
        Style::Percent => put_str(vm, o, "style", "percent")?,
        Style::Currency { code, display, sign } => {
            put_str(vm, o, "style", "currency")?;
            put_str(vm, o, "currency", code)?;
            put_str(
                vm,
                o,
                "currencyDisplay",
                match display {
                    CurrencyDisplay::Code => "code",
                    CurrencyDisplay::Symbol => "symbol",
                    CurrencyDisplay::NarrowSymbol => "narrowSymbol",
                    CurrencyDisplay::Name => "name",
                },
            )?;
            put_str(vm, o, "currencySign", if *sign == CurrencySign::Accounting { "accounting" } else { "standard" })?;
        }
        Style::Unit { unit, display } => {
            put_str(vm, o, "style", "unit")?;
            put_str(vm, o, "unit", unit)?;
            put_str(
                vm,
                o,
                "unitDisplay",
                match display {
                    UnitDisplay::Short => "short",
                    UnitDisplay::Narrow => "narrow",
                    UnitDisplay::Long => "long",
                },
            )?;
        }
    }
    put_digit_options(vm, o, &opts.digits, &priority, true)?;
    if use_grouping == "false" {
        put_value(vm, o, "useGrouping", Value::Bool(false))?;
    } else {
        put_str(vm, o, "useGrouping", &use_grouping)?;
    }
    put_str(
        vm,
        o,
        "notation",
        match opts.notation {
            Notation::Standard => "standard",
            Notation::Scientific => "scientific",
            Notation::Engineering => "engineering",
            Notation::Compact { .. } => "compact",
        },
    )?;
    if let Some(cd) = compact_display {
        put_str(vm, o, "compactDisplay", &cd)?;
    }
    put_str(
        vm,
        o,
        "signDisplay",
        match opts.sign_display {
            SignDisplay::Auto => "auto",
            SignDisplay::Never => "never",
            SignDisplay::Always => "always",
            SignDisplay::ExceptZero => "exceptZero",
            SignDisplay::Negative => "negative",
        },
    )?;
    put_value(vm, o, "roundingIncrement", Value::Number(opts.digits.rounding_increment as f64))?;
    put_str(vm, o, "roundingMode", rounding_mode_name(opts.digits.rounding_mode))?;
    put_str(vm, o, "roundingPriority", &priority)?;
    put_str(vm, o, "trailingZeroDisplay", if opts.digits.trailing_zero_display == TrailingZeroDisplay::StripIfInteger { "stripIfInteger" } else { "auto" })?;
    Ok(Value::Object(o))
}

/// Number.prototype.toLocaleString and BigInt.prototype.toLocaleString:
/// format `x` with a fresh `Intl.NumberFormat(locales, options)`.
pub fn to_locale_string(vm: &mut Vm, x: Value, locales: Value, options: Value) -> JsResult<Value> {
    let ctor = vm.intrinsic(Intrinsic::IntlNumberFormat);
    let nf = vm.construct_value(Value::Object(ctor), &[locales, options], Value::Object(ctor))?;
    let Value::Object(nf) = nf else { unreachable!() };
    let num = num_of(vm, x)?;
    let s = record(vm, nf).formatter.format(&num);
    Ok(vm.str(&s))
}
