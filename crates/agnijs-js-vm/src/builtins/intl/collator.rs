//! `Intl.Collator` (ECMA-402 §10) and `String.prototype.localeCompare`.

use agnijs_js_intl::collator::{CollatorOptionsIn, IntlCollator, Sensitivity};

use crate::object::{CallArgs, Class};
use crate::realm::Intrinsic;
use crate::value::{ObjId, Value};
use crate::vm::{JsResult, Vm};

use super::abstract_ops::*;
use super::{create_intl_object, intl_data, put_str, put_value, IntlData};

pub struct CollatorRecord {
    pub collator: IntlCollator,
    pub locale: String,
    pub usage: String,
    pub sensitivity: Sensitivity,
    pub ignore_punctuation: bool,
    pub collation: String,
    pub numeric: bool,
    pub case_first: String,
    pub bound_compare: Option<ObjId>,
}

pub fn install(vm: &mut Vm, intl: ObjId) {
    let (_ctor, proto) = super::install_service(
        vm,
        intl,
        "Collator",
        0,
        constructor,
        Intrinsic::IntlCollator,
        Intrinsic::IntlCollatorPrototype,
        supported_locales_of,
    );
    vm.accessor(proto, "compare", compare_getter, None);
    vm.method(proto, "resolvedOptions", 0, resolved_options);
}

fn collation_data(locale: &str, usage: &str) -> Vec<String> {
    let mut v = vec!["default".to_string()];
    if usage == "search" {
        return v;
    }
    for c in agnijs_js_intl::data::collations_for(locale) {
        if c != "default" && c != "standard" && c != "search" && !v.contains(&c) {
            v.push(c);
        }
    }
    v
}

/// Intl.Collator([locales [, options]])
fn constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let new_target = if a.new_target.is_undefined() { Value::Object(a.callee) } else { a.new_target };
    let requested = canonicalize_locale_list(vm, a.arg(0))?;
    let options = coerce_options_to_object(vm, a.arg(1))?;
    let usage = get_string_option(vm, options, "usage", &["sort", "search"])?.unwrap_or_else(|| "sort".to_string());
    get_locale_matcher(vm, options)?;
    let collation = get_string_option(vm, options, "collation", &[])?;
    if let Some(c) = &collation {
        if !is_unicode_type(c) {
            return vm.range_error(&format!("Invalid collation {c}"));
        }
    }
    let numeric = get_bool_option(vm, options, "numeric")?.map(|b| b.to_string());
    let case_first = get_string_option(vm, options, "caseFirst", &["upper", "lower", "false"])?;
    let usage_c = usage.clone();
    let r = resolve_locale(
        &requested,
        &["co", "kf", "kn"],
        &[collation, case_first, numeric],
        &|loc, key| match key {
            "co" => collation_data(loc, &usage_c),
            "kf" => vec!["false".into(), "lower".into(), "upper".into()],
            _ => vec!["false".into(), "true".into()],
        },
        vm,
    );
    let resolved_collation = r.keywords[0].clone().unwrap_or_else(|| "default".to_string());
    let resolved_kf = r.keywords[1].clone().unwrap_or_else(|| "false".to_string());
    let resolved_kn = r.keywords[2].as_deref() == Some("true");
    let sensitivity = get_string_option(vm, options, "sensitivity", &["base", "accent", "case", "variant"])?;
    let sensitivity = match sensitivity {
        Some(s) => Sensitivity::parse(&s).unwrap(),
        None => Sensitivity::Variant,
    };
    let ignore_punctuation = get_bool_option(vm, options, "ignorePunctuation")?;
    // The seam collator takes the collation through the locale tag.
    let mut seam_locale = r.data_locale.clone();
    if resolved_collation != "default" {
        seam_locale = agnijs_js_intl::locale::with_unicode_keywords(&seam_locale, &[("co", Some(resolved_collation.as_str()))]);
    }
    if usage == "search" {
        seam_locale = agnijs_js_intl::locale::with_unicode_keywords(&seam_locale, &[("co", Some("search"))]);
    }
    // Default ignorePunctuation follows the locale (Thai ignores it).
    let collator = match IntlCollator::new(&CollatorOptionsIn {
        locale: seam_locale.clone(),
        search: usage == "search",
        sensitivity,
        ignore_punctuation,
        numeric: resolved_kn,
        case_first: resolved_kf.clone(),
    }) {
        Ok(c) => c,
        Err(e) => return vm.range_error(&e),
    };
    let ignore = collator.ignores_punctuation();
    let record = CollatorRecord {
        collator,
        locale: r.locale,
        usage,
        sensitivity,
        ignore_punctuation: ignore,
        collation: resolved_collation,
        numeric: resolved_kn,
        case_first: resolved_kf,
        bound_compare: None,
    };
    let args = CallArgs { this: a.this, args: a.args, new_target, callee: a.callee };
    let obj = create_intl_object(vm, &args, Intrinsic::IntlCollatorPrototype, IntlData::Collator(Box::new(record)))?;
    Ok(Value::Object(obj))
}

fn this_collator(vm: &mut Vm, this: Value, method: &str) -> JsResult<ObjId> {
    match intl_data(vm, this) {
        Some(IntlData::Collator(_)) => Ok(this.as_object().unwrap()),
        _ => vm.type_error(&format!("Intl.Collator.prototype.{method} called on an incompatible receiver")),
    }
}

fn record(vm: &Vm, o: ObjId) -> &CollatorRecord {
    match &vm.heap.obj(o).class {
        Class::Intl(d) => match &**d {
            IntlData::Collator(r) => r,
            _ => unreachable!(),
        },
        _ => unreachable!(),
    }
}

fn compare_getter(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = this_collator(vm, a.this, "compare")?;
    if let Some(f) = record(vm, o).bound_compare {
        return Ok(Value::Object(f));
    }
    let f = vm.new_native("", 2, bound_compare);
    if let Class::Native(n) = &mut vm.heap.obj_mut(f).class {
        n.slots.push(Value::Object(o));
    }
    if let Class::Intl(d) = &mut vm.heap.obj_mut(o).class {
        if let IntlData::Collator(r) = &mut **d {
            r.bound_compare = Some(f);
        }
    }
    Ok(Value::Object(f))
}

/// CompareStrings(collator, x, y) as a JS number.
pub fn compare_values(vm: &mut Vm, collator: ObjId, x: Value, y: Value) -> JsResult<Value> {
    let xs = vm.to_string(x)?;
    let ys = vm.to_string(y)?;
    let xu = vm.heap.units(xs).to_vec();
    let yu = vm.heap.units(ys).to_vec();
    let ord = record(vm, collator).collator.compare(&xu, &yu);
    Ok(Value::Number(match ord {
        std::cmp::Ordering::Less => -1.0,
        std::cmp::Ordering::Equal => 0.0,
        std::cmp::Ordering::Greater => 1.0,
    }))
}

fn bound_compare(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let c = match &vm.heap.obj(a.callee).class {
        Class::Native(n) => n.slots[0].as_object().unwrap(),
        _ => unreachable!(),
    };
    compare_values(vm, c, a.arg(0), a.arg(1))
}

fn resolved_options(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let c = this_collator(vm, a.this, "resolvedOptions")?;
    let o = vm.new_object();
    let (locale, usage, sens, punct, collation, numeric, kf) = {
        let r = record(vm, c);
        (r.locale.clone(), r.usage.clone(), r.sensitivity, r.ignore_punctuation, r.collation.clone(), r.numeric, r.case_first.clone())
    };
    put_str(vm, o, "locale", &locale)?;
    put_str(vm, o, "usage", &usage)?;
    put_str(vm, o, "sensitivity", sens.name())?;
    put_value(vm, o, "ignorePunctuation", Value::Bool(punct))?;
    put_str(vm, o, "collation", &collation)?;
    put_value(vm, o, "numeric", Value::Bool(numeric))?;
    put_str(vm, o, "caseFirst", &kf)?;
    Ok(Value::Object(o))
}

/// String.prototype.localeCompare(that [, locales [, options]])
pub fn locale_compare(vm: &mut Vm, s: Value, that: Value, locales: Value, options: Value) -> JsResult<Value> {
    let ctor = vm.intrinsic(Intrinsic::IntlCollator);
    let c = vm.construct_value(Value::Object(ctor), &[locales, options], Value::Object(ctor))?;
    let Value::Object(c) = c else { unreachable!() };
    compare_values(vm, c, s, that)
}
