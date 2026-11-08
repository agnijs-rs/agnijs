//! `Intl.RelativeTimeFormat` (ECMA-402 §17).

use gable_js_intl::relativetime::{IntlRelativeTimeFormatter, RelStyle, RelUnit};

use crate::object::{CallArgs, Class};
use crate::realm::Intrinsic;
use crate::value::{ObjId, Value};
use crate::vm::{JsResult, Vm};

use super::abstract_ops::*;
use super::{create_intl_object, intl_data, put_str, IntlData};

pub struct RelativeTimeFormatRecord {
    pub formatter: IntlRelativeTimeFormatter,
    pub locale: String,
    pub numbering_system: String,
    pub style: String,
    pub numeric: String,
}

pub fn install(vm: &mut Vm, intl: ObjId) {
    let (_ctor, proto) = super::install_service(
        vm,
        intl,
        "RelativeTimeFormat",
        0,
        constructor,
        Intrinsic::IntlRelativeTimeFormat,
        Intrinsic::IntlRelativeTimeFormatPrototype,
        supported_locales_of,
    );
    vm.method(proto, "format", 2, format);
    vm.method(proto, "formatToParts", 2, format_to_parts);
    vm.method(proto, "resolvedOptions", 0, resolved_options);
}

fn constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if !a.is_construct() {
        return vm.type_error("Intl.RelativeTimeFormat must be called with new");
    }
    let requested = canonicalize_locale_list(vm, a.arg(0))?;
    let options = coerce_options_to_object(vm, a.arg(1))?;
    get_locale_matcher(vm, options)?;
    let nu = get_string_option(vm, options, "numberingSystem", &[])?;
    if let Some(n) = &nu {
        if !is_unicode_type(n) {
            return vm.range_error(&format!("Invalid numberingSystem {n}"));
        }
    }
    let r = resolve_locale(&requested, &["nu"], &[nu], &|loc, _| super::numberformat::numbering_system_data(loc), vm);
    let numbering_system = r.keywords[0].clone().unwrap_or_else(|| "latn".to_string());
    let style = get_string_option(vm, options, "style", &["long", "short", "narrow"])?.unwrap_or_else(|| "long".to_string());
    let numeric = get_string_option(vm, options, "numeric", &["always", "auto"])?.unwrap_or_else(|| "always".to_string());
    let seam_style = match style.as_str() {
        "short" => RelStyle::Short,
        "narrow" => RelStyle::Narrow,
        _ => RelStyle::Long,
    };
    let formatter = match IntlRelativeTimeFormatter::new(&r.locale, &numbering_system, seam_style, numeric == "auto") {
        Ok(f) => f,
        Err(e) => return vm.range_error(&e),
    };
    let record = RelativeTimeFormatRecord { formatter, locale: r.locale, numbering_system, style, numeric };
    let obj = create_intl_object(vm, a, Intrinsic::IntlRelativeTimeFormatPrototype, IntlData::RelativeTimeFormat(Box::new(record)))?;
    Ok(Value::Object(obj))
}

fn this_rtf(vm: &mut Vm, this: Value, method: &str) -> JsResult<ObjId> {
    match intl_data(vm, this) {
        Some(IntlData::RelativeTimeFormat(_)) => Ok(this.as_object().unwrap()),
        _ => vm.type_error(&format!("Intl.RelativeTimeFormat.prototype.{method} called on an incompatible receiver")),
    }
}

fn record(vm: &Vm, o: ObjId) -> &RelativeTimeFormatRecord {
    match &vm.heap.obj(o).class {
        Class::Intl(d) => match &**d {
            IntlData::RelativeTimeFormat(r) => r,
            _ => unreachable!(),
        },
        _ => unreachable!(),
    }
}

/// The checked (value, unit) pair of PartitionRelativeTimePattern.
fn inputs(vm: &mut Vm, a: &CallArgs) -> JsResult<(f64, RelUnit)> {
    let value = vm.to_number(a.arg(0))?;
    let unit = vm.to_rust_string(a.arg(1))?;
    if !value.is_finite() {
        return vm.range_error("Value must be finite");
    }
    match RelUnit::parse(&unit) {
        Some(u) => Ok((value, u)),
        None => vm.range_error(&format!("Invalid unit argument for format() '{unit}'")),
    }
}

fn format(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let rtf = this_rtf(vm, a.this, "format")?;
    let (value, unit) = inputs(vm, a)?;
    match record(vm, rtf).formatter.format(value, unit) {
        Ok(s) => Ok(vm.str(&s)),
        Err(e) => vm.range_error(&e),
    }
}

fn format_to_parts(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let rtf = this_rtf(vm, a.this, "formatToParts")?;
    let (value, unit) = inputs(vm, a)?;
    let parts = match record(vm, rtf).formatter.format_to_parts(value, unit) {
        Ok(p) => p,
        Err(e) => return vm.range_error(&e),
    };
    let mut items = Vec::with_capacity(parts.len());
    for p in &parts {
        let o = vm.new_object();
        put_str(vm, o, "type", p.kind)?;
        put_str(vm, o, "value", &p.value)?;
        if let Some(u) = p.unit {
            put_str(vm, o, "unit", u)?;
        }
        items.push(Value::Object(o));
    }
    Ok(Value::Object(vm.new_array(items)))
}

fn resolved_options(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let rtf = this_rtf(vm, a.this, "resolvedOptions")?;
    let o = vm.new_object();
    let (locale, style, numeric, nu) = {
        let r = record(vm, rtf);
        (r.locale.clone(), r.style.clone(), r.numeric.clone(), r.numbering_system.clone())
    };
    put_str(vm, o, "locale", &locale)?;
    put_str(vm, o, "style", &style)?;
    put_str(vm, o, "numeric", &numeric)?;
    put_str(vm, o, "numberingSystem", &nu)?;
    Ok(Value::Object(o))
}
