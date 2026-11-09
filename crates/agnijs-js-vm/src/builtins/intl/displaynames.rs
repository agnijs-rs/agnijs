//! `Intl.DisplayNames` (ECMA-402 §12).

use agnijs_js_intl::displaynames::{IntlDisplayNames, Lookup, NameStyle, NameType};

use crate::object::{CallArgs, Class};
use crate::realm::Intrinsic;
use crate::value::{ObjId, Value};
use crate::vm::{JsResult, Vm};

use super::abstract_ops::*;
use super::{intl_data, put_str, IntlData};

pub struct DisplayNamesRecord {
    pub names: IntlDisplayNames,
    pub locale: String,
    pub style: String,
    pub ty: String,
    pub fallback: String,
    pub language_display: String,
}

pub fn install(vm: &mut Vm, intl: ObjId) {
    let (_ctor, proto) = super::install_service(
        vm,
        intl,
        "DisplayNames",
        2,
        constructor,
        Intrinsic::IntlDisplayNames,
        Intrinsic::IntlDisplayNamesPrototype,
        supported_locales_of,
    );
    vm.method(proto, "of", 1, of);
    vm.method(proto, "resolvedOptions", 0, resolved_options);
}

fn constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if !a.is_construct() {
        return vm.type_error("Intl.DisplayNames must be called with new");
    }
    let proto = vm.get_prototype_from_constructor(a.new_target, Intrinsic::IntlDisplayNamesPrototype)?;
    let requested = canonicalize_locale_list(vm, a.arg(0))?;
    if a.arg(1).is_undefined() {
        return vm.type_error("Intl.DisplayNames requires an options object");
    }
    let options = get_options_object(vm, a.arg(1))?;
    get_locale_matcher(vm, options)?;
    let r = resolve_locale(&requested, &[], &[], &|_, _| Vec::new(), vm);
    let style = get_string_option(vm, options, "style", &["narrow", "short", "long"])?.unwrap_or_else(|| "long".to_string());
    let ty = get_string_option(vm, options, "type", &["language", "region", "script", "currency", "calendar", "dateTimeField"])?;
    let Some(ty) = ty else { return vm.type_error("Intl.DisplayNames requires a type option") };
    let fallback = get_string_option(vm, options, "fallback", &["code", "none"])?.unwrap_or_else(|| "code".to_string());
    let language_display = get_string_option(vm, options, "languageDisplay", &["dialect", "standard"])?.unwrap_or_else(|| "dialect".to_string());
    let seam_style = match style.as_str() {
        "narrow" => NameStyle::Narrow,
        "short" => NameStyle::Short,
        _ => NameStyle::Long,
    };
    let names = match IntlDisplayNames::new(&r.locale, NameType::parse(&ty).unwrap(), seam_style, language_display == "dialect") {
        Ok(n) => n,
        Err(e) => return vm.range_error(&e),
    };
    let record = DisplayNamesRecord { names, locale: r.locale, style, ty, fallback, language_display };
    let obj = vm.alloc(crate::object::ObjectData::new(Some(proto), Class::Intl(Box::new(IntlData::DisplayNames(Box::new(record))))));
    Ok(Value::Object(obj))
}

fn this_dn(vm: &mut Vm, this: Value, method: &str) -> JsResult<ObjId> {
    match intl_data(vm, this) {
        Some(IntlData::DisplayNames(_)) => Ok(this.as_object().unwrap()),
        _ => vm.type_error(&format!("Intl.DisplayNames.prototype.{method} called on an incompatible receiver")),
    }
}

fn record(vm: &Vm, o: ObjId) -> &DisplayNamesRecord {
    match &vm.heap.obj(o).class {
        Class::Intl(d) => match &**d {
            IntlData::DisplayNames(r) => r,
            _ => unreachable!(),
        },
        _ => unreachable!(),
    }
}

fn of(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let dn = this_dn(vm, a.this, "of")?;
    let code = vm.to_rust_string(a.arg(0))?;
    let (lookup, fallback) = {
        let r = record(vm, dn);
        (r.names.of(&code), r.fallback.clone())
    };
    match lookup {
        Lookup::Name(n) => Ok(vm.str(&n)),
        Lookup::Code(c) => {
            if fallback == "code" {
                Ok(vm.str(&c))
            } else {
                Ok(Value::Undefined)
            }
        }
        Lookup::Invalid => vm.range_error(&format!("invalid_argument: {code}")),
    }
}

fn resolved_options(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let dn = this_dn(vm, a.this, "resolvedOptions")?;
    let o = vm.new_object();
    let (locale, style, ty, fallback, ld) = {
        let r = record(vm, dn);
        (r.locale.clone(), r.style.clone(), r.ty.clone(), r.fallback.clone(), r.language_display.clone())
    };
    put_str(vm, o, "locale", &locale)?;
    put_str(vm, o, "style", &style)?;
    put_str(vm, o, "type", &ty)?;
    put_str(vm, o, "fallback", &fallback)?;
    if ty == "language" {
        put_str(vm, o, "languageDisplay", &ld)?;
    }
    Ok(Value::Object(o))
}
