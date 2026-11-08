//! `Intl.Locale` (ECMA-402 §14) plus the Locale Info methods.

use crate::object::CallArgs;
use crate::realm::Intrinsic;
use crate::value::{ObjId, Value};
use crate::vm::{JsResult, Vm};

use super::abstract_ops::*;
use super::{create_intl_object, intl_data, put_value, string_list_to_array, IntlData};
use gable_js_intl::locale as seam;

pub struct LocaleRecord {
    pub locale: String,
    pub calendar: Option<String>,
    pub case_first: Option<String>,
    pub collation: Option<String>,
    pub first_day_of_week: Option<String>,
    pub hour_cycle: Option<String>,
    pub numeric: bool,
    pub numbering_system: Option<String>,
}

const RELEVANT_KEYS: [&str; 7] = ["ca", "co", "fw", "hc", "kf", "kn", "nu"];

pub fn install(vm: &mut Vm, intl: ObjId) {
    let proto = vm.new_object();
    let ctor = vm.new_native_ctor("Locale", 1, constructor);
    vm.realm_mut().set_intrinsic(Intrinsic::IntlLocale, ctor);
    vm.realm_mut().set_intrinsic(Intrinsic::IntlLocalePrototype, proto);
    vm.define_raw_str(ctor, "prototype", Value::Object(proto), crate::value::Attrs::NONE);
    vm.define_raw_str(proto, "constructor", Value::Object(ctor), crate::value::Attrs::METHOD);
    vm.define_raw_str(intl, "Locale", Value::Object(ctor), crate::value::Attrs::METHOD);
    super::super::to_string_tag(vm, proto, "Intl.Locale");
    vm.method(proto, "maximize", 0, maximize);
    vm.method(proto, "minimize", 0, minimize);
    vm.method(proto, "toString", 0, to_string);
    vm.method(proto, "getCalendars", 0, get_calendars);
    vm.method(proto, "getCollations", 0, get_collations);
    vm.method(proto, "getHourCycles", 0, get_hour_cycles);
    vm.method(proto, "getNumberingSystems", 0, get_numbering_systems);
    vm.method(proto, "getTimeZones", 0, get_time_zones);
    vm.method(proto, "getTextInfo", 0, get_text_info);
    vm.method(proto, "getWeekInfo", 0, get_week_info);
    vm.accessor(proto, "baseName", base_name, None);
    vm.accessor(proto, "calendar", calendar, None);
    vm.accessor(proto, "caseFirst", case_first, None);
    vm.accessor(proto, "collation", collation, None);
    vm.accessor(proto, "firstDayOfWeek", first_day_of_week, None);
    vm.accessor(proto, "hourCycle", hour_cycle, None);
    vm.accessor(proto, "numeric", numeric, None);
    vm.accessor(proto, "numberingSystem", numbering_system, None);
    vm.accessor(proto, "language", language, None);
    vm.accessor(proto, "script", script, None);
    vm.accessor(proto, "region", region, None);
    vm.accessor(proto, "variants", variants, None);
}

fn this_locale<'a>(vm: &'a mut Vm, this: Value, method: &str) -> JsResult<&'a LocaleRecord> {
    match intl_data(vm, this) {
        Some(IntlData::Locale(_)) => {}
        _ => return vm.type_error(&format!("Intl.Locale.prototype.{method} called on an incompatible receiver")),
    }
    match intl_data(vm, this) {
        Some(IntlData::Locale(r)) => Ok(r),
        _ => unreachable!(),
    }
}

/// WeekdayToString(fw) for the `firstDayOfWeek` option.
fn weekday_to_string(s: &str) -> String {
    match s {
        "0" | "7" => "sun".into(),
        "1" => "mon".into(),
        "2" => "tue".into(),
        "3" => "wed".into(),
        "4" => "thu".into(),
        "5" => "fri".into(),
        "6" => "sat".into(),
        other => other.to_string(),
    }
}

fn is_variant_subtag(s: &str) -> bool {
    let b = s.as_bytes();
    (b.len() >= 5 && b.len() <= 8 && b.iter().all(u8::is_ascii_alphanumeric))
        || (b.len() == 4 && b[0].is_ascii_digit() && b.iter().all(u8::is_ascii_alphanumeric))
}

/// UpdateLanguageId(tag, options): apply `language`, `script`, `region`
/// and `variants` to a structurally valid tag.
fn update_language_id(vm: &mut Vm, tag: &str, options: ObjId) -> JsResult<String> {
    let language = get_string_option(vm, options, "language", &[])?;
    if let Some(l) = &language {
        if !seam::is_language_subtag(l) || l.len() == 4 {
            return vm.range_error(&format!("Invalid language subtag {l}"));
        }
    }
    let script = get_string_option(vm, options, "script", &[])?;
    if let Some(s) = &script {
        if !seam::is_script_subtag(s) {
            return vm.range_error(&format!("Invalid script subtag {s}"));
        }
    }
    let region = get_string_option(vm, options, "region", &[])?;
    if let Some(r) = &region {
        if !seam::is_region_subtag(r) {
            return vm.range_error(&format!("Invalid region subtag {r}"));
        }
    }
    let variants = get_string_option(vm, options, "variants", &[])?;
    if let Some(v) = &variants {
        let mut seen: Vec<String> = Vec::new();
        if v.is_empty() {
            return vm.range_error("Invalid variants option");
        }
        for sub in v.split('-') {
            let lower = sub.to_ascii_lowercase();
            if !is_variant_subtag(sub) || seen.contains(&lower) {
                return vm.range_error(&format!("Invalid variant subtag {sub}"));
            }
            seen.push(lower);
        }
    }
    let Some(canonical) = seam::canonicalize(tag) else {
        return vm.range_error(&format!("Invalid language tag: {tag}"));
    };
    let (base, _) = seam::split_unicode_extension(&canonical);
    let p = seam::parts(&base);
    // Rebuild the language id, then re-attach everything after it.
    let new_language = language.as_deref().unwrap_or(&p.language).to_ascii_lowercase();
    let new_script = script.as_deref().map(|s| s.to_string()).or(p.script.clone());
    let new_region = region.as_deref().map(|s| s.to_string()).or(p.region.clone());
    let new_variants: Vec<String> = match &variants {
        Some(v) => v.split('-').map(|s| s.to_string()).collect(),
        None => p.variants.clone(),
    };
    let mut id = new_language;
    if let Some(s) = new_script {
        id.push('-');
        id.push_str(&s);
    }
    if let Some(r) = new_region {
        id.push('-');
        id.push_str(&r);
    }
    for v in new_variants {
        id.push('-');
        id.push_str(&v);
    }
    // Everything after the old language id (extensions and private use).
    let old_id_len = {
        let mut n = p.language.len();
        if let Some(s) = &p.script {
            n += 1 + s.len();
        }
        if let Some(r) = &p.region {
            n += 1 + r.len();
        }
        for v in &p.variants {
            n += 1 + v.len();
        }
        n
    };
    let rest = &canonical[old_id_len.min(canonical.len())..];
    let joined = format!("{id}{rest}");
    match seam::canonicalize(&joined) {
        Some(c) => Ok(c),
        None => vm.range_error(&format!("Invalid language tag: {joined}")),
    }
}

/// Intl.Locale(tag [, options])
fn constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if !a.is_construct() {
        return vm.type_error("Intl.Locale must be called with new");
    }
    let tag_v = a.arg(0);
    if !tag_v.is_string() && !tag_v.is_object() {
        return vm.type_error("First argument to Intl.Locale must be a string or object");
    }
    let tag = match intl_data(vm, tag_v) {
        Some(IntlData::Locale(r)) => r.locale.clone(),
        _ => vm.to_rust_string(tag_v)?,
    };
    let options = coerce_options_to_object(vm, a.arg(1))?;
    if !seam::is_structurally_valid(&tag) {
        return vm.range_error(&format!("Invalid language tag: {tag}"));
    }
    let tag = update_language_id(vm, &tag, options)?;

    let calendar = get_string_option(vm, options, "calendar", &[])?;
    if let Some(c) = &calendar {
        if !is_unicode_type(c) {
            return vm.range_error(&format!("Invalid calendar {c}"));
        }
    }
    let collation = get_string_option(vm, options, "collation", &[])?;
    if let Some(c) = &collation {
        if !is_unicode_type(c) {
            return vm.range_error(&format!("Invalid collation {c}"));
        }
    }
    let fw = get_string_option(vm, options, "firstDayOfWeek", &[])?;
    let fw = fw.map(|s| weekday_to_string(&s));
    if let Some(f) = &fw {
        if !is_unicode_type(f) {
            return vm.range_error(&format!("Invalid firstDayOfWeek {f}"));
        }
    }
    let hc = get_string_option(vm, options, "hourCycle", &["h11", "h12", "h23", "h24"])?;
    let kf = get_string_option(vm, options, "caseFirst", &["upper", "lower", "false"])?;
    let kn = get_bool_option(vm, options, "numeric")?.map(|b| b.to_string());
    let nu = get_string_option(vm, options, "numberingSystem", &[])?;
    if let Some(n) = &nu {
        if !is_unicode_type(n) {
            return vm.range_error(&format!("Invalid numberingSystem {n}"));
        }
    }
    let mut opts = [calendar, collation, fw, hc, kf, kn, nu];
    for (i, key) in RELEVANT_KEYS.iter().enumerate() {
        if let Some(v) = &opts[i] {
            let c = seam::canonicalize_keyword_value(key, v);
            opts[i] = Some(if c.is_empty() { "true".to_string() } else { c });
        }
    }

    // MakeLocaleRecord: merge the tag's keywords with the options.
    let (base, keywords) = seam::split_unicode_extension(&tag);
    let mut values: Vec<Option<String>> = Vec::new();
    let mut new_keywords: Vec<(String, Option<String>)> = Vec::new();
    for (i, key) in RELEVANT_KEYS.iter().enumerate() {
        // Keyword values are canonical (`kf-true` is `kf`, stored as the
        // empty string, which the getters report as "" / `numeric` true).
        let mut value = keywords.iter().find(|(k, _)| k == key).map(|(_, v)| seam::canonicalize_keyword_value(key, v));
        if let Some(o) = &opts[i] {
            value = Some(if o == "true" { String::new() } else { o.clone() });
        }
        if let Some(v) = &value {
            new_keywords.push((key.to_string(), Some(v.clone())));
        }
        values.push(value);
    }
    let kw_refs: Vec<(&str, Option<&str>)> = new_keywords.iter().map(|(k, v)| (k.as_str(), v.as_deref())).collect();
    let locale = seam::with_unicode_keywords(&tag, &kw_refs);
    let locale = seam::canonicalize(&locale).unwrap_or(locale);
    let _ = base;
    let record = LocaleRecord {
        locale,
        calendar: values[0].clone(),
        collation: values[1].clone(),
        first_day_of_week: values[2].clone(),
        hour_cycle: values[3].clone(),
        case_first: values[4].clone(),
        numeric: values[5].as_deref() == Some(""),
        numbering_system: values[6].clone(),
    };
    let obj = create_intl_object(vm, a, Intrinsic::IntlLocalePrototype, IntlData::Locale(Box::new(record)))?;
    Ok(Value::Object(obj))
}

fn construct_from_tag(vm: &mut Vm, tag: &str) -> Value {
    let ctor = vm.intrinsic(Intrinsic::IntlLocale);
    let t = vm.str(tag);
    vm.construct_value(Value::Object(ctor), &[t], Value::Object(ctor)).unwrap_or(Value::Undefined)
}

fn maximize(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let loc = this_locale(vm, a.this, "maximize")?.locale.clone();
    let max = seam::maximize(&loc);
    Ok(construct_from_tag(vm, &max))
}

fn minimize(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let loc = this_locale(vm, a.this, "minimize")?.locale.clone();
    let min = seam::minimize(&loc);
    Ok(construct_from_tag(vm, &min))
}

fn to_string(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let loc = this_locale(vm, a.this, "toString")?.locale.clone();
    Ok(vm.str(&loc))
}

fn opt_str(vm: &mut Vm, v: &Option<String>) -> Value {
    match v {
        Some(s) => vm.str(s),
        None => Value::Undefined,
    }
}

fn base_name(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let loc = this_locale(vm, a.this, "baseName")?.locale.clone();
    let b = seam::base_name(&loc);
    // base_name drops all extensions; the spec's baseName is the
    // unicode_language_id, which is the same thing.
    Ok(vm.str(&b))
}

fn calendar(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let v = this_locale(vm, a.this, "calendar")?.calendar.clone();
    Ok(opt_str(vm, &v))
}
fn case_first(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let v = this_locale(vm, a.this, "caseFirst")?.case_first.clone();
    Ok(opt_str(vm, &v))
}
fn collation(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let v = this_locale(vm, a.this, "collation")?.collation.clone();
    Ok(opt_str(vm, &v))
}
fn first_day_of_week(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let v = this_locale(vm, a.this, "firstDayOfWeek")?.first_day_of_week.clone();
    Ok(opt_str(vm, &v))
}
fn hour_cycle(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let v = this_locale(vm, a.this, "hourCycle")?.hour_cycle.clone();
    Ok(opt_str(vm, &v))
}
fn numeric(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let v = this_locale(vm, a.this, "numeric")?.numeric;
    Ok(Value::Bool(v))
}
fn numbering_system(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let v = this_locale(vm, a.this, "numberingSystem")?.numbering_system.clone();
    Ok(opt_str(vm, &v))
}
fn language(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let loc = this_locale(vm, a.this, "language")?.locale.clone();
    let p = seam::parts(&loc);
    Ok(vm.str(&p.language))
}
fn script(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let loc = this_locale(vm, a.this, "script")?.locale.clone();
    let p = seam::parts(&loc);
    Ok(opt_str(vm, &p.script))
}
fn region(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let loc = this_locale(vm, a.this, "region")?.locale.clone();
    let p = seam::parts(&loc);
    Ok(opt_str(vm, &p.region))
}
fn variants(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let loc = this_locale(vm, a.this, "variants")?.locale.clone();
    let p = seam::parts(&loc);
    if p.variants.is_empty() {
        return Ok(Value::Undefined);
    }
    Ok(vm.str(&p.variants.join("-")))
}

/// CreateArrayFromListAndPreferred: the preferred value alone when the
/// locale asks for one, else the whole list.
fn list_or_preferred(vm: &mut Vm, list: Vec<String>, preferred: &Option<String>) -> Value {
    match preferred {
        Some(p) => string_list_to_array(vm, &[p.clone()]),
        None => string_list_to_array(vm, &list),
    }
}

fn get_calendars(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let r = this_locale(vm, a.this, "getCalendars")?;
    let (loc, pref) = (r.locale.clone(), r.calendar.clone());
    let list = gable_js_intl::data::calendars_for(&loc);
    Ok(list_or_preferred(vm, list, &pref))
}
fn get_collations(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let r = this_locale(vm, a.this, "getCollations")?;
    let (loc, pref) = (r.locale.clone(), r.collation.clone());
    let list: Vec<String> = gable_js_intl::data::collations_for(&loc)
        .into_iter()
        .filter(|c| c != "standard" && c != "search")
        .collect();
    let pref = pref.filter(|p| p != "standard" && p != "search");
    let mut out = match pref {
        Some(p) => vec![p],
        None => list,
    };
    out.sort();
    Ok(string_list_to_array(vm, &out))
}
fn get_hour_cycles(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let r = this_locale(vm, a.this, "getHourCycles")?;
    let (loc, pref) = (r.locale.clone(), r.hour_cycle.clone());
    let list = gable_js_intl::data::hour_cycles_for(&loc);
    let first = vec![list[0].clone()];
    Ok(list_or_preferred(vm, first, &pref))
}
fn get_numbering_systems(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let r = this_locale(vm, a.this, "getNumberingSystems")?;
    let (loc, pref) = (r.locale.clone(), r.numbering_system.clone());
    let list = gable_js_intl::data::numbering_systems_for(&loc);
    let first = vec![list[0].clone()];
    Ok(list_or_preferred(vm, first, &pref))
}
fn get_time_zones(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let loc = this_locale(vm, a.this, "getTimeZones")?.locale.clone();
    let p = seam::parts(&loc);
    let Some(region) = p.region else { return Ok(Value::Undefined) };
    let mut zones = gable_js_intl::timezone::zones_for_region(&region).unwrap_or_default();
    zones.sort();
    Ok(string_list_to_array(vm, &zones))
}
fn get_text_info(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let loc = this_locale(vm, a.this, "getTextInfo")?.locale.clone();
    let rtl = seam::is_right_to_left(&loc);
    let info = vm.new_object();
    let dir = vm.str(if rtl { "rtl" } else { "ltr" });
    put_value(vm, info, "direction", dir)?;
    Ok(Value::Object(info))
}
fn get_week_info(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let r = this_locale(vm, a.this, "getWeekInfo")?;
    let (loc, fw) = (r.locale.clone(), r.first_day_of_week.clone());
    let (mut first, mut weekend) = gable_js_intl::data::week_info(&loc);
    if let Some(fw) = fw {
        if let Some(d) = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"].iter().position(|d| *d == fw) {
            first = d as u8 + 1;
        }
    }
    weekend.sort();
    let info = vm.new_object();
    put_value(vm, info, "firstDay", Value::Number(first as f64))?;
    let items: Vec<Value> = weekend.iter().map(|d| Value::Number(*d as f64)).collect();
    let arr = vm.new_array(items);
    put_value(vm, info, "weekend", Value::Object(arr))?;
    Ok(Value::Object(info))
}
