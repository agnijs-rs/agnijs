//! ECMA-402 §9: locale and option abstract operations shared by every
//! service constructor.

use crate::object::CallArgs;
use crate::value::{ObjId, Value};
use crate::vm::{JsResult, Vm};

use super::{intl_data, IntlData};

/// DefaultLocale(): the host's locale, canonicalized, or `en-US` when the
/// host gives something invalid or unsupported.
pub fn default_locale(vm: &Vm) -> String {
    let tag = vm.host.default_locale();
    if agnijs_js_intl::locale::is_structurally_valid(&tag) {
        if let Some(c) = agnijs_js_intl::locale::canonicalize(&tag) {
            if agnijs_js_intl::locale::is_available(&c) {
                return c;
            }
        }
    }
    "en-US".to_string()
}

/// DefaultTimeZone(): the host's zone canonicalized, or `UTC`.
pub fn default_time_zone(vm: &Vm) -> String {
    let tz = vm.host.default_time_zone();
    agnijs_js_intl::timezone::canonicalize(&tz).unwrap_or_else(|| "UTC".to_string())
}

/// CanonicalizeLocaleList(locales)
pub fn canonicalize_locale_list(vm: &mut Vm, locales: Value) -> JsResult<Vec<String>> {
    if locales.is_undefined() {
        return Ok(Vec::new());
    }
    let mut seen: Vec<String> = Vec::new();
    let o = if locales.is_string() || matches!(intl_data(vm, locales), Some(IntlData::Locale(_))) {
        vm.new_array(vec![locales])
    } else {
        vm.to_object(locales)?
    };
    let len = vm.length_of_array_like(o)?;
    let mut k = 0.0;
    while k < len {
        let pk = vm.number_key(k);
        if vm.has_property(o, pk)? {
            let kv = vm.get(o, pk)?;
            if !kv.is_string() && !kv.is_object() {
                return vm.type_error("Locale list entries must be strings or objects");
            }
            let tag = match intl_data(vm, kv) {
                Some(IntlData::Locale(r)) => r.locale.clone(),
                _ => vm.to_rust_string(kv)?,
            };
            if !agnijs_js_intl::locale::is_structurally_valid(&tag) {
                return vm.range_error(&format!("Invalid language tag: {tag}"));
            }
            let Some(canonical) = agnijs_js_intl::locale::canonicalize(&tag) else {
                return vm.range_error(&format!("Invalid language tag: {tag}"));
            };
            if !seen.contains(&canonical) {
                seen.push(canonical);
            }
        }
        k += 1.0;
    }
    Ok(seen)
}

/// CoerceOptionsToObject(options)
pub fn coerce_options_to_object(vm: &mut Vm, options: Value) -> JsResult<ObjId> {
    if options.is_undefined() {
        return Ok(vm.new_object_with_proto(None));
    }
    vm.to_object(options)
}

/// GetOptionsObject(options)
pub fn get_options_object(vm: &mut Vm, options: Value) -> JsResult<ObjId> {
    if options.is_undefined() {
        return Ok(vm.new_object_with_proto(None));
    }
    if let Value::Object(o) = options {
        return Ok(o);
    }
    vm.type_error("Options must be an object or undefined")
}

/// GetOption(options, property, string, values, fallback) for a string
/// option. An empty `values` accepts any string.
pub fn get_string_option(vm: &mut Vm, options: ObjId, name: &str, values: &[&str]) -> JsResult<Option<String>> {
    let v = vm.get_str(options, name)?;
    if v.is_undefined() {
        return Ok(None);
    }
    let s = vm.to_rust_string(v)?;
    if !values.is_empty() && !values.contains(&s.as_str()) {
        return vm.range_error(&format!("Value {s} out of range for option {name}"));
    }
    Ok(Some(s))
}

/// GetOption(options, property, boolean, empty, fallback).
pub fn get_bool_option(vm: &mut Vm, options: ObjId, name: &str) -> JsResult<Option<bool>> {
    let v = vm.get_str(options, name)?;
    if v.is_undefined() {
        return Ok(None);
    }
    Ok(Some(vm.to_boolean(v)))
}

/// GetBooleanOrStringNumberFormatOption(options, property, stringValues,
/// fallback): `true`, `false`, or one of the strings.
pub enum BoolOrString {
    Bool(bool),
    Str(String),
}

pub fn get_bool_or_string_option(
    vm: &mut Vm,
    options: ObjId,
    name: &str,
    values: &[&str],
    fallback: BoolOrString,
) -> JsResult<BoolOrString> {
    let v = vm.get_str(options, name)?;
    if v.is_undefined() {
        return Ok(fallback);
    }
    if v == Value::Bool(true) {
        return Ok(BoolOrString::Bool(true));
    }
    if !vm.to_boolean(v) {
        return Ok(BoolOrString::Bool(false));
    }
    let s = vm.to_rust_string(v)?;
    if s == "true" || s == "false" {
        return Ok(fallback);
    }
    if !values.contains(&s.as_str()) {
        return vm.range_error(&format!("Value {s} out of range for option {name}"));
    }
    Ok(BoolOrString::Str(s))
}

/// DefaultNumberOption(value, minimum, maximum, fallback)
pub fn default_number_option(vm: &mut Vm, v: Value, min: f64, max: f64, fallback: Option<f64>) -> JsResult<Option<f64>> {
    if v.is_undefined() {
        return Ok(fallback);
    }
    let n = vm.to_number(v)?;
    if n.is_nan() || n < min || n > max {
        return vm.range_error(&format!("Value {n} out of range"));
    }
    Ok(Some(n.floor()))
}

/// GetNumberOption(options, property, minimum, maximum, fallback)
pub fn get_number_option(vm: &mut Vm, options: ObjId, name: &str, min: f64, max: f64, fallback: Option<f64>) -> JsResult<Option<f64>> {
    let v = vm.get_str(options, name)?;
    match default_number_option(vm, v, min, max, fallback) {
        Ok(r) => Ok(r),
        Err(_) => {
            let n = vm.to_number(v).unwrap_or(f64::NAN);
            vm.range_error(&format!("{name} value is out of range: {n}"))
        }
    }
}

/// `localeMatcher` option, consumed by every constructor and
/// `supportedLocalesOf`.
pub fn get_locale_matcher(vm: &mut Vm, options: ObjId) -> JsResult<()> {
    get_string_option(vm, options, "localeMatcher", &["lookup", "best fit"])?;
    Ok(())
}

/// A `[-a-z0-9]{3,8}(-[a-z0-9]{3,8})*` Unicode type value (case folded).
pub fn is_unicode_type(s: &str) -> bool {
    agnijs_js_intl::locale::is_unicode_type(s)
}

/// BestAvailableLocale(availableLocales, locale)
pub fn best_available_locale(locale: &str) -> Option<String> {
    let mut candidate = locale.to_string();
    loop {
        if agnijs_js_intl::locale::is_available(&candidate) {
            return Some(candidate);
        }
        let Some(pos) = candidate.rfind('-') else { return None };
        let mut cut = pos;
        if cut >= 2 && candidate.as_bytes()[cut - 2] == b'-' {
            cut -= 2;
        }
        candidate.truncate(cut);
    }
}

/// The result of ResolveLocale.
pub struct ResolvedLocale {
    /// The locale with the supported extension keywords reinserted.
    pub locale: String,
    /// The locale the data was found for (no extension).
    pub data_locale: String,
    /// One entry per relevant extension key, in order.
    pub keywords: Vec<Option<String>>,
}

/// ResolveLocale(availableLocales, requestedLocales, options,
/// relevantExtensionKeys, localeData). `options` carries the already
/// coerced option value per key (strings; `""` means `true`);
/// `locale_data(found, key)` lists the supported values with the default
/// first.
pub fn resolve_locale(
    requested: &[String],
    relevant_keys: &[&str],
    options: &[Option<String>],
    locale_data: &dyn Fn(&str, &str) -> Vec<String>,
    vm: &Vm,
) -> ResolvedLocale {
    // LookupMatchingLocaleByPrefix (best fit is the same for us).
    let mut found: Option<(String, Vec<(String, String)>, Vec<String>)> = None;
    for tag in requested {
        let (base, keywords) = agnijs_js_intl::locale::split_unicode_extension(tag);
        if let Some(avail) = best_available_locale(&base) {
            // Attributes of the Unicode extension are not part of the
            // resolved locale (only supported keywords are re-inserted).
            found = Some((avail, keywords, Vec::new()));
            break;
        }
    }
    let (found_locale, ext_keywords, attributes) = found.unwrap_or_else(|| (default_locale(vm), Vec::new(), Vec::new()));
    let mut result_keywords = Vec::with_capacity(relevant_keys.len());
    let mut supported: Vec<(String, String)> = Vec::new();
    for (i, key) in relevant_keys.iter().enumerate() {
        let key_data = locale_data(&found_locale, key);
        let mut value = key_data.first().cloned();
        let mut supported_keyword: Option<(String, String)> = None;
        if let Some((_, requested_value)) = ext_keywords.iter().find(|(k, _)| k == key) {
            if !requested_value.is_empty() {
                if key_data.iter().any(|v| v == requested_value) {
                    value = Some(requested_value.clone());
                    supported_keyword = Some((key.to_string(), requested_value.clone()));
                }
            } else if key_data.iter().any(|v| v == "true") {
                value = Some("true".to_string());
                supported_keyword = Some((key.to_string(), String::new()));
            }
        }
        if let Some(Some(opt)) = options.get(i) {
            let opt = agnijs_js_intl::locale::canonicalize_keyword_value(key, opt);
            let opt = if opt.is_empty() { "true".to_string() } else { opt };
            if key_data.iter().any(|v| *v == opt) && value.as_deref() != Some(opt.as_str()) {
                value = Some(opt);
                supported_keyword = None;
            }
        }
        result_keywords.push(value);
        if let Some(kw) = supported_keyword {
            supported.push(kw);
        }
    }
    let mut locale = found_locale.clone();
    if !supported.is_empty() || !attributes.is_empty() {
        let mut ext = String::from("-u");
        for a in &attributes {
            ext.push('-');
            ext.push_str(a);
        }
        for (k, v) in &supported {
            ext.push('-');
            ext.push_str(k);
            if !v.is_empty() {
                ext.push('-');
                ext.push_str(v);
            }
        }
        let tag = format!("{found_locale}{ext}");
        locale = agnijs_js_intl::locale::canonicalize(&tag).unwrap_or(tag);
    }
    ResolvedLocale { locale, data_locale: found_locale, keywords: result_keywords }
}


/// LookupSupportedLocales(availableLocales, requestedLocales)
pub fn lookup_supported_locales(requested: &[String]) -> Vec<String> {
    requested
        .iter()
        .filter(|tag| {
            let (base, _) = agnijs_js_intl::locale::split_unicode_extension(tag);
            best_available_locale(&base).is_some()
        })
        .cloned()
        .collect()
}

/// SupportedLocales(availableLocales, requestedLocales, options): the
/// shared body of every `supportedLocalesOf`.
pub fn supported_locales_of(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let requested = canonicalize_locale_list(vm, a.arg(0))?;
    let options = coerce_options_to_object(vm, a.arg(1))?;
    get_locale_matcher(vm, options)?;
    let list = lookup_supported_locales(&requested);
    Ok(super::string_list_to_array(vm, &list))
}

/// Whether `s` matches `(3*8alphanum) *("-" (3*8alphanum))`, the
/// numbering-system and calendar option syntax.
pub fn is_well_formed_type_option(s: &str) -> bool {
    is_unicode_type(s)
}

/// Finite number formatting input, as the seam wants it.
pub fn num_of(vm: &mut Vm, v: Value) -> JsResult<agnijs_js_intl::number::Num> {
    use agnijs_js_intl::decimal::Dec;
    use agnijs_js_intl::number::Num;
    match v {
        Value::BigInt(b) => {
            let s = vm.heap.bigint(b).0.to_string();
            Ok(Num::Finite(Dec::parse(&s).unwrap_or_else(Dec::zero)))
        }
        _ => {
            let n = vm.to_number(v)?;
            Ok(Num::from_f64(n))
        }
    }
}

/// ToIntlMathematicalValue(value): numbers, BigInts and numeric strings
/// (decimal, hex, binary, octal, Infinity) as exact decimals.
pub fn to_intl_mathematical_value(vm: &mut Vm, v: Value) -> JsResult<agnijs_js_intl::number::Num> {
    use agnijs_js_intl::decimal::Dec;
    use agnijs_js_intl::number::Num;
    let prim = vm.to_primitive(v, crate::convert::Hint::Number)?;
    match prim {
        Value::BigInt(b) => {
            let s = vm.heap.bigint(b).0.to_string();
            Ok(Num::Finite(Dec::parse(&s).unwrap_or_else(Dec::zero)))
        }
        Value::String(s) => {
            let text = vm.heap.str_to_string(s);
            let t = text.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
            if t.is_empty() {
                return Ok(Num::Finite(Dec::zero()));
            }
            let (neg, body) = match t.as_bytes()[0] {
                b'-' => (true, &t[1..]),
                b'+' => (false, &t[1..]),
                _ => (false, t),
            };
            if body == "Infinity" {
                return Ok(if neg { Num::NegInf } else { Num::PosInf });
            }
            if let Some(mut d) = Dec::parse(t) {
                d.strip_trailing_zeros();
                if d.is_zero() {
                    d.neg = neg;
                }
                return Ok(Num::Finite(d));
            }
            // Non-decimal literals and anything else go through ToNumber.
            let n = vm.to_number(Value::String(s))?;
            Ok(Num::from_f64(n))
        }
        other => {
            let n = vm.to_number(other)?;
            Ok(Num::from_f64(n))
        }
    }
}
