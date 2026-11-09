//! `Intl.DateTimeFormat` (ECMA-402 §11) and the `Date.prototype.toLocale*`
//! methods built on it.

use agnijs_js_intl::datetime::{Components, DateTimeFormatOptions, DateTimeFormatter};

use crate::object::{CallArgs, Class};
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, Value};
use crate::vm::{JsResult, Vm};

use super::abstract_ops::*;
use super::numberformat::{parts_to_array, range_parts_to_array};
use super::{intl_data, put_str, put_value, IntlData};

pub struct DateTimeFormatRecord {
    pub formatter: DateTimeFormatter,
    pub locale: String,
    pub calendar: String,
    pub numbering_system: String,
    pub time_zone: String,
    /// `None` when the pattern has no hour.
    pub hour_cycle: Option<String>,
    pub date_style: Option<String>,
    pub time_style: Option<String>,
    pub bound_format: Option<ObjId>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Required {
    Date,
    Time,
    Any,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Defaults {
    Date,
    Time,
    All,
}

pub fn install(vm: &mut Vm, intl: ObjId) {
    let (_ctor, proto) = super::install_service(
        vm,
        intl,
        "DateTimeFormat",
        0,
        constructor,
        Intrinsic::IntlDateTimeFormat,
        Intrinsic::IntlDateTimeFormatPrototype,
        supported_locales_of,
    );
    vm.accessor(proto, "format", format_getter, None);
    vm.method(proto, "formatToParts", 1, format_to_parts);
    vm.method(proto, "formatRange", 2, format_range);
    vm.method(proto, "formatRangeToParts", 2, format_range_to_parts);
    vm.method(proto, "resolvedOptions", 0, resolved_options);
}

/// The `ca` locale data: the locale's preferred calendars, then the rest.
fn calendar_data(locale: &str) -> Vec<String> {
    let mut v = agnijs_js_intl::data::calendars_for(locale);
    for c in agnijs_js_intl::data::CALENDARS {
        if !v.iter().any(|x| x == c) {
            v.push(c.to_string());
        }
    }
    v
}

fn hour_cycle_data(locale: &str) -> Vec<String> {
    agnijs_js_intl::data::hour_cycles_for(locale)
}

const COMPONENT_NAMES: [&str; 11] = ["weekday", "era", "year", "month", "day", "dayPeriod", "hour", "minute", "second", "fractionalSecondDigits", "timeZoneName"];

fn component_values(name: &str) -> &'static [&'static str] {
    match name {
        "weekday" | "era" | "dayPeriod" => &["narrow", "short", "long"],
        "month" => &["2-digit", "numeric", "narrow", "short", "long"],
        "timeZoneName" => &["short", "long", "shortOffset", "longOffset", "shortGeneric", "longGeneric"],
        _ => &["2-digit", "numeric"],
    }
}

/// CreateDateTimeFormat(newTarget, locales, options, required, defaults)
pub fn create_date_time_format(
    vm: &mut Vm,
    new_target: Value,
    locales: Value,
    options: Value,
    required: Required,
    defaults: Defaults,
) -> JsResult<ObjId> {
    let proto = vm.get_prototype_from_constructor(new_target, Intrinsic::IntlDateTimeFormatPrototype)?;
    let requested = canonicalize_locale_list(vm, locales)?;
    let options = coerce_options_to_object(vm, options)?;
    get_locale_matcher(vm, options)?;
    let calendar = get_string_option(vm, options, "calendar", &[])?;
    if let Some(c) = &calendar {
        if !is_unicode_type(c) {
            return vm.range_error(&format!("Invalid calendar {c}"));
        }
    }
    let numbering_system = get_string_option(vm, options, "numberingSystem", &[])?;
    if let Some(n) = &numbering_system {
        if !is_unicode_type(n) {
            return vm.range_error(&format!("Invalid numberingSystem {n}"));
        }
    }
    let hour12 = get_bool_option(vm, options, "hour12")?;
    let mut hour_cycle_opt = get_string_option(vm, options, "hourCycle", &["h11", "h12", "h23", "h24"])?;
    if hour12.is_some() {
        hour_cycle_opt = None;
    }
    let calendar_opt = calendar.map(|c| agnijs_js_intl::data::canonical_calendar(&c));
    let r = resolve_locale(
        &requested,
        &["ca", "hc", "nu"],
        &[calendar_opt, hour_cycle_opt.clone(), numbering_system],
        &|loc, key| match key {
            "ca" => calendar_data(loc),
            "hc" => hour_cycle_data(loc),
            _ => super::numberformat::numbering_system_data(loc),
        },
        vm,
    );
    // An explicit hour12 option makes the hc option null: the tag's own
    // `hc` keyword is dropped from the resolved locale.
    let mut r = r;
    if hour12.is_some() {
        r.locale = agnijs_js_intl::locale::with_unicode_keywords(&r.locale, &[("hc", None)]);
        r.keywords[1] = None;
    }
    let resolved_calendar = r.keywords[0].clone().unwrap_or_else(|| "gregory".to_string());
    let resolved_nu = r.keywords[2].clone().unwrap_or_else(|| "latn".to_string());
    let hc_default = hour_cycle_data(&r.data_locale)[0].clone();

    // Time zone.
    let tz_v = vm.get_str(options, "timeZone")?;
    let time_zone = if tz_v.is_undefined() {
        default_time_zone(vm)
    } else {
        let s = vm.to_rust_string(tz_v)?;
        if let Some(offset_ms) = agnijs_js_intl::timezone::parse_offset_zone(&s) {
            agnijs_js_intl::timezone::format_offset_zone(offset_ms)
        } else if let Some(c) = agnijs_js_intl::timezone::canonicalize(&s) {
            c
        } else {
            return vm.range_error(&format!("Invalid time zone specified: {s}"));
        }
    };

    // Component options, in table order.
    let mut comps = Components::default();
    let mut has_explicit = false;
    for name in COMPONENT_NAMES {
        if name == "fractionalSecondDigits" {
            let v = get_number_option(vm, options, name, 1.0, 3.0, None)?;
            if let Some(d) = v {
                comps.fractional_second_digits = Some(d as u8);
                has_explicit = true;
            }
            continue;
        }
        let v = get_string_option(vm, options, name, component_values(name))?;
        if v.is_some() {
            has_explicit = true;
        }
        match name {
            "weekday" => comps.weekday = v,
            "era" => comps.era = v,
            "year" => comps.year = v,
            "month" => comps.month = v,
            "day" => comps.day = v,
            "dayPeriod" => comps.day_period = v,
            "hour" => comps.hour = v,
            "minute" => comps.minute = v,
            "second" => comps.second = v,
            "timeZoneName" => comps.time_zone_name = v,
            _ => {}
        }
    }
    get_string_option(vm, options, "formatMatcher", &["basic", "best fit"])?;
    let date_style = get_string_option(vm, options, "dateStyle", &["full", "long", "medium", "short"])?;
    let time_style = get_string_option(vm, options, "timeStyle", &["full", "long", "medium", "short"])?;
    if date_style.is_some() || time_style.is_some() {
        if has_explicit {
            return vm.type_error("dateStyle and timeStyle cannot be combined with component options");
        }
        if required == Required::Date && time_style.is_some() {
            return vm.type_error("timeStyle is not allowed here");
        }
        if required == Required::Time && date_style.is_some() {
            return vm.type_error("dateStyle is not allowed here");
        }
    } else {
        let mut need_defaults = true;
        if matches!(required, Required::Date | Required::Any) && (comps.weekday.is_some() || comps.year.is_some() || comps.month.is_some() || comps.day.is_some()) {
            need_defaults = false;
        }
        if matches!(required, Required::Time | Required::Any)
            && (comps.day_period.is_some() || comps.hour.is_some() || comps.minute.is_some() || comps.second.is_some() || comps.fractional_second_digits.is_some())
        {
            need_defaults = false;
        }
        if need_defaults && matches!(defaults, Defaults::Date | Defaults::All) {
            comps.year = Some("numeric".into());
            comps.month = Some("numeric".into());
            comps.day = Some("numeric".into());
        }
        if need_defaults && matches!(defaults, Defaults::Time | Defaults::All) {
            comps.hour = Some("numeric".into());
            comps.minute = Some("numeric".into());
            comps.second = Some("numeric".into());
        }
    }

    // Hour cycle.
    let has_hour = comps.hour.is_some() || time_style.is_some();
    // ES2025 §11.1.2 steps 41-42: hour12 true picks h11 only where the
    // locale itself uses h11, false is always h23.
    let hc = match hour12 {
        Some(true) => {
            if hc_default == "h11" || hc_default == "h12" {
                hc_default.clone()
            } else {
                agnijs_js_intl::data::hour_cycle_12(&r.data_locale).to_string()
            }
        }
        Some(false) => "h23".to_string(),
        None => r.keywords[1].clone().unwrap_or(hc_default),
    };
    let hc_for_locale = if has_hour { Some(hc.clone()) } else { None };
    let seam_locale = agnijs_js_intl::locale::with_unicode_keywords(
        &r.data_locale,
        &[("ca", Some(resolved_calendar.as_str())), ("nu", Some(resolved_nu.as_str())), ("hc", hc_for_locale.as_deref())],
    );
    let seam_opts = DateTimeFormatOptions {
        locale: seam_locale,
        time_zone: time_zone.clone(),
        components: comps,
        date_style: date_style.clone(),
        time_style: time_style.clone(),
    };
    let formatter = match DateTimeFormatter::new(&seam_opts) {
        Ok(f) => f,
        Err(e) => return vm.range_error(&e),
    };
    let resolved_hc = formatter.resolved().hour_cycle.map(|s| s.to_string());
    let record = DateTimeFormatRecord {
        formatter,
        locale: r.locale,
        calendar: resolved_calendar,
        numbering_system: resolved_nu,
        time_zone,
        hour_cycle: if has_hour { resolved_hc.or(Some(hc)) } else { None },
        date_style,
        time_style,
        bound_format: None,
    };
    Ok(vm.alloc(crate::object::ObjectData::new(Some(proto), Class::Intl(Box::new(IntlData::DateTimeFormat(Box::new(record)))))))
}

fn constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let new_target = if a.new_target.is_undefined() { Value::Object(a.callee) } else { a.new_target };
    let obj = create_date_time_format(vm, new_target, a.arg(0), a.arg(1), Required::Any, Defaults::Date)?;
    if a.new_target.is_undefined() {
        if let Value::Object(this) = a.this {
            let ctor = vm.intrinsic(Intrinsic::IntlDateTimeFormat);
            if vm.ordinary_has_instance(Value::Object(ctor), a.this)? {
                let key = crate::value::PropKey::Sym(vm.realm().intl_fallback);
                vm.define_property_or_throw(this, key, crate::value::PropertyDescriptor::data(Value::Object(obj), Attrs::NONE))?;
                return Ok(a.this);
            }
        }
    }
    Ok(Value::Object(obj))
}

fn is_dtf(vm: &Vm, o: ObjId) -> bool {
    matches!(&vm.heap.obj(o).class, Class::Intl(d) if matches!(**d, IntlData::DateTimeFormat(_)))
}

/// UnwrapDateTimeFormat(dtf)
fn unwrap(vm: &mut Vm, this: Value, method: &str) -> JsResult<ObjId> {
    if let Value::Object(o) = this {
        if is_dtf(vm, o) {
            return Ok(o);
        }
        let ctor = vm.intrinsic(Intrinsic::IntlDateTimeFormat);
        if vm.ordinary_has_instance(Value::Object(ctor), this)? {
            let key = crate::value::PropKey::Sym(vm.realm().intl_fallback);
            if let Value::Object(inner) = vm.get(o, key)? {
                if is_dtf(vm, inner) {
                    return Ok(inner);
                }
            }
        }
    }
    vm.type_error(&format!("Intl.DateTimeFormat.prototype.{method} called on an incompatible receiver"))
}

fn this_dtf(vm: &mut Vm, this: Value, method: &str) -> JsResult<ObjId> {
    match intl_data(vm, this) {
        Some(IntlData::DateTimeFormat(_)) => Ok(this.as_object().unwrap()),
        _ => vm.type_error(&format!("Intl.DateTimeFormat.prototype.{method} called on an incompatible receiver")),
    }
}

pub fn record(vm: &Vm, o: ObjId) -> &DateTimeFormatRecord {
    match &vm.heap.obj(o).class {
        Class::Intl(d) => match &**d {
            IntlData::DateTimeFormat(r) => r,
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
        if let IntlData::DateTimeFormat(r) = &mut **d {
            r.bound_format = Some(f);
        }
    }
    Ok(Value::Object(f))
}

/// The time value to format: now when absent, else ToNumber + TimeClip.
fn time_arg(vm: &mut Vm, v: Value) -> JsResult<f64> {
    let t = if v.is_undefined() { vm.host.now_ms() } else { vm.to_number(v)? };
    let t = time_clip(t);
    if t.is_nan() {
        return vm.range_error("Invalid time value");
    }
    Ok(t)
}

fn time_clip(t: f64) -> f64 {
    if !t.is_finite() || t.abs() > 8.64e15 {
        return f64::NAN;
    }
    t.trunc() + 0.0
}

fn bound_format(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let dtf = match &vm.heap.obj(a.callee).class {
        Class::Native(n) => n.slots[0].as_object().unwrap(),
        _ => unreachable!(),
    };
    let t = time_arg(vm, a.arg(0))?;
    let s = record(vm, dtf).formatter.format(t);
    Ok(vm.str(&s))
}

fn format_to_parts(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let dtf = this_dtf(vm, a.this, "formatToParts")?;
    let t = time_arg(vm, a.arg(0))?;
    let parts = record(vm, dtf).formatter.format_to_parts(t);
    parts_to_array(vm, &parts, None)
}

fn range_args(vm: &mut Vm, a: &CallArgs) -> JsResult<(f64, f64)> {
    if a.arg(0).is_undefined() || a.arg(1).is_undefined() {
        return vm.type_error("formatRange requires a start and an end date");
    }
    let x = time_arg(vm, a.arg(0))?;
    let y = time_arg(vm, a.arg(1))?;
    Ok((x, y))
}

fn format_range(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let dtf = this_dtf(vm, a.this, "formatRange")?;
    let (x, y) = range_args(vm, a)?;
    let s = record(vm, dtf).formatter.format_range(x, y);
    Ok(vm.str(&s))
}

fn format_range_to_parts(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let dtf = this_dtf(vm, a.this, "formatRangeToParts")?;
    let (x, y) = range_args(vm, a)?;
    let parts = record(vm, dtf).formatter.format_range_to_parts(x, y);
    range_parts_to_array(vm, &parts)
}

fn resolved_options(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let dtf = unwrap(vm, a.this, "resolvedOptions")?;
    let o = vm.new_object();
    let (locale, calendar, nu, tz, hc, ds, ts, comps) = {
        let r = record(vm, dtf);
        (
            r.locale.clone(),
            r.calendar.clone(),
            r.numbering_system.clone(),
            r.time_zone.clone(),
            r.hour_cycle.clone(),
            r.date_style.clone(),
            r.time_style.clone(),
            r.formatter.resolved().components.clone(),
        )
    };
    put_str(vm, o, "locale", &locale)?;
    put_str(vm, o, "calendar", &calendar)?;
    put_str(vm, o, "numberingSystem", &nu)?;
    put_str(vm, o, "timeZone", &tz)?;
    if let Some(hc) = &hc {
        put_str(vm, o, "hourCycle", hc)?;
        put_value(vm, o, "hour12", Value::Bool(hc == "h11" || hc == "h12"))?;
    }
    if ds.is_none() && ts.is_none() {
        let fields: [(&str, &Option<String>); 10] = [
            ("weekday", &comps.weekday),
            ("era", &comps.era),
            ("year", &comps.year),
            ("month", &comps.month),
            ("day", &comps.day),
            ("dayPeriod", &comps.day_period),
            ("hour", &comps.hour),
            ("minute", &comps.minute),
            ("second", &comps.second),
            ("timeZoneName", &comps.time_zone_name),
        ];
        for (name, v) in fields {
            if let Some(v) = v {
                put_str(vm, o, name, v)?;
            }
            if name == "second" {
                if let Some(d) = comps.fractional_second_digits {
                    put_value(vm, o, "fractionalSecondDigits", Value::Number(d as f64))?;
                }
            }
        }
    } else {
        if let Some(ds) = &ds {
            put_str(vm, o, "dateStyle", ds)?;
        }
        if let Some(ts) = &ts {
            put_str(vm, o, "timeStyle", ts)?;
        }
    }
    Ok(Value::Object(o))
}

/// Date.prototype.toLocaleString / toLocaleDateString / toLocaleTimeString
/// for a valid time value `t`.
pub fn date_to_locale_string(vm: &mut Vm, t: f64, locales: Value, options: Value, required: Required, defaults: Defaults) -> JsResult<Value> {
    let ctor = vm.intrinsic(Intrinsic::IntlDateTimeFormat);
    let dtf = create_date_time_format(vm, Value::Object(ctor), locales, options, required, defaults)?;
    let s = record(vm, dtf).formatter.format(t);
    Ok(vm.str(&s))
}
