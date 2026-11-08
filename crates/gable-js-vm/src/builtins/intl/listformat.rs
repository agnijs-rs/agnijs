//! `Intl.ListFormat` (ECMA-402 §13).

use gable_js_intl::list::{IntlListFormatter, ListStyle, ListType};

use crate::object::{CallArgs, Class};
use crate::realm::Intrinsic;
use crate::value::{ObjId, Value};
use crate::vm::{JsResult, Vm};

use super::abstract_ops::*;
use super::numberformat::parts_to_array;
use super::{create_intl_object, intl_data, put_str, IntlData};

pub struct ListFormatRecord {
    pub formatter: IntlListFormatter,
    pub locale: String,
    pub ty: String,
    pub style: String,
}

pub fn install(vm: &mut Vm, intl: ObjId) {
    let (_ctor, proto) = super::install_service(
        vm,
        intl,
        "ListFormat",
        0,
        constructor,
        Intrinsic::IntlListFormat,
        Intrinsic::IntlListFormatPrototype,
        supported_locales_of,
    );
    vm.method(proto, "format", 1, format);
    vm.method(proto, "formatToParts", 1, format_to_parts);
    vm.method(proto, "resolvedOptions", 0, resolved_options);
}

fn constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if !a.is_construct() {
        return vm.type_error("Intl.ListFormat must be called with new");
    }
    let requested = canonicalize_locale_list(vm, a.arg(0))?;
    let options = get_options_object(vm, a.arg(1))?;
    get_locale_matcher(vm, options)?;
    let r = resolve_locale(&requested, &[], &[], &|_, _| Vec::new(), vm);
    let ty = get_string_option(vm, options, "type", &["conjunction", "disjunction", "unit"])?.unwrap_or_else(|| "conjunction".to_string());
    let style = get_string_option(vm, options, "style", &["long", "short", "narrow"])?.unwrap_or_else(|| "long".to_string());
    let seam_ty = match ty.as_str() {
        "disjunction" => ListType::Disjunction,
        "unit" => ListType::Unit,
        _ => ListType::Conjunction,
    };
    let seam_style = match style.as_str() {
        "short" => ListStyle::Short,
        "narrow" => ListStyle::Narrow,
        _ => ListStyle::Long,
    };
    let formatter = match IntlListFormatter::new(&r.locale, seam_ty, seam_style) {
        Ok(f) => f,
        Err(e) => return vm.range_error(&e),
    };
    let record = ListFormatRecord { formatter, locale: r.locale, ty, style };
    let obj = create_intl_object(vm, a, Intrinsic::IntlListFormatPrototype, IntlData::ListFormat(Box::new(record)))?;
    Ok(Value::Object(obj))
}

fn this_lf(vm: &mut Vm, this: Value, method: &str) -> JsResult<ObjId> {
    match intl_data(vm, this) {
        Some(IntlData::ListFormat(_)) => Ok(this.as_object().unwrap()),
        _ => vm.type_error(&format!("Intl.ListFormat.prototype.{method} called on an incompatible receiver")),
    }
}

fn record(vm: &Vm, o: ObjId) -> &ListFormatRecord {
    match &vm.heap.obj(o).class {
        Class::Intl(d) => match &**d {
            IntlData::ListFormat(r) => r,
            _ => unreachable!(),
        },
        _ => unreachable!(),
    }
}

/// StringListFromIterable(iterable)
fn string_list_from_iterable(vm: &mut Vm, iterable: Value) -> JsResult<Vec<String>> {
    if iterable.is_undefined() {
        return Ok(Vec::new());
    }
    let (it, next) = vm.get_iterator(iterable, false)?;
    let mut out = Vec::new();
    while let Some(v) = vm.iterator_step_value(it, next)? {
        if !v.is_string() {
            vm.iterator_close_swallow(it);
            return vm.type_error("Iterable yielded a non-string value");
        }
        out.push(vm.to_rust_string(v)?);
    }
    Ok(out)
}

fn format(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let lf = this_lf(vm, a.this, "format")?;
    let list = string_list_from_iterable(vm, a.arg(0))?;
    let s = record(vm, lf).formatter.format(&list);
    Ok(vm.str(&s))
}

fn format_to_parts(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let lf = this_lf(vm, a.this, "formatToParts")?;
    let list = string_list_from_iterable(vm, a.arg(0))?;
    let parts = record(vm, lf).formatter.format_to_parts(&list);
    parts_to_array(vm, &parts, None)
}

fn resolved_options(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let lf = this_lf(vm, a.this, "resolvedOptions")?;
    let o = vm.new_object();
    let (locale, ty, style) = {
        let r = record(vm, lf);
        (r.locale.clone(), r.ty.clone(), r.style.clone())
    };
    put_str(vm, o, "locale", &locale)?;
    put_str(vm, o, "type", &ty)?;
    put_str(vm, o, "style", &style)?;
    Ok(Value::Object(o))
}
