//! String constructor and String.prototype.

use crate::builtins::iterator::create_string_iterator;
use crate::convert::to_integer_or_infinity_f64;
use crate::object::{CallArgs, Class};
use crate::realm::Intrinsic;
use crate::string::{is_js_whitespace, StrBuilder, Units};
use crate::value::{Attrs, ObjId, PropKey, StrId, Value};
use crate::vm::{JsResult, Vm};
use agnijs_engine_api::RealmId;

pub fn install(vm: &mut Vm, realm: RealmId) {
    let object_proto = vm.intrinsic(Intrinsic::ObjectPrototype);
    let empty = vm.names.empty;
    let proto = vm.string_object(empty, object_proto);
    let ctor = super::install_ctor(
        vm,
        realm,
        "String",
        1,
        string_constructor,
        proto,
        Intrinsic::String,
        Intrinsic::StringPrototype,
    );
    vm.method(ctor, "fromCharCode", 1, string_from_char_code);
    vm.method(ctor, "fromCodePoint", 1, string_from_code_point);
    vm.method(ctor, "raw", 1, string_raw);

    vm.method(proto, "at", 1, proto_at);
    vm.method(proto, "charAt", 1, proto_char_at);
    vm.method(proto, "charCodeAt", 1, proto_char_code_at);
    vm.method(proto, "codePointAt", 1, proto_code_point_at);
    vm.method(proto, "concat", 1, proto_concat);
    vm.method(proto, "endsWith", 1, proto_ends_with);
    vm.method(proto, "includes", 1, proto_includes);
    vm.method(proto, "indexOf", 1, proto_index_of);
    vm.method(proto, "isWellFormed", 0, proto_is_well_formed);
    vm.method(proto, "lastIndexOf", 1, proto_last_index_of);
    vm.method(proto, "localeCompare", 1, proto_locale_compare);
    vm.method(proto, "match", 1, proto_match);
    vm.method(proto, "matchAll", 1, proto_match_all);
    vm.method(proto, "normalize", 0, proto_normalize);
    vm.method(proto, "padEnd", 1, proto_pad_end);
    vm.method(proto, "padStart", 1, proto_pad_start);
    vm.method(proto, "repeat", 1, proto_repeat);
    vm.method(proto, "replace", 2, proto_replace);
    vm.method(proto, "replaceAll", 2, proto_replace_all);
    vm.method(proto, "search", 1, proto_search);
    vm.method(proto, "slice", 2, proto_slice);
    vm.method(proto, "split", 2, proto_split);
    vm.method(proto, "startsWith", 1, proto_starts_with);
    vm.method(proto, "substring", 2, proto_substring);
    vm.method(proto, "toLocaleLowerCase", 0, proto_to_locale_lower_case);
    vm.method(proto, "toLocaleUpperCase", 0, proto_to_locale_upper_case);
    vm.method(proto, "toLowerCase", 0, proto_to_lower_case);
    vm.method(proto, "toString", 0, proto_value_of);
    vm.method(proto, "toUpperCase", 0, proto_to_upper_case);
    vm.method(proto, "toWellFormed", 0, proto_to_well_formed);
    vm.method(proto, "trim", 0, proto_trim);
    let trim_end = vm.method(proto, "trimEnd", 0, proto_trim_end);
    let trim_start = vm.method(proto, "trimStart", 0, proto_trim_start);
    vm.method(proto, "valueOf", 0, proto_value_of);
    let iter = vm.well_known.iterator;
    vm.method_sym(proto, iter, "Symbol.iterator", 0, proto_iterator);

    // Annex B.
    vm.method(proto, "substr", 2, proto_substr);
    vm.define_raw_str(proto, "trimLeft", Value::Object(trim_start), Attrs::METHOD);
    vm.define_raw_str(proto, "trimRight", Value::Object(trim_end), Attrs::METHOD);
    vm.method(proto, "anchor", 1, html_anchor);
    vm.method(proto, "big", 0, html_big);
    vm.method(proto, "blink", 0, html_blink);
    vm.method(proto, "bold", 0, html_bold);
    vm.method(proto, "fixed", 0, html_fixed);
    vm.method(proto, "fontcolor", 1, html_fontcolor);
    vm.method(proto, "fontsize", 1, html_fontsize);
    vm.method(proto, "italics", 0, html_italics);
    vm.method(proto, "link", 1, html_link);
    vm.method(proto, "small", 0, html_small);
    vm.method(proto, "strike", 0, html_strike);
    vm.method(proto, "sub", 0, html_sub);
    vm.method(proto, "sup", 0, html_sup);
}

// ---- helpers -------------------------------------------------------------------------

/// RequireObjectCoercible(this) then ToString.
fn this_str(vm: &mut Vm, a: &CallArgs, name: &str) -> JsResult<StrId> {
    if a.this.is_nullish() {
        return vm.type_error(&format!(
            "String.prototype.{name} called on null or undefined"
        ));
    }
    vm.to_string(a.this)
}

fn units_of(vm: &mut Vm, s: StrId) -> Vec<u16> {
    vm.heap.units(s).to_vec()
}

fn this_string_value(vm: &mut Vm, v: Value) -> JsResult<StrId> {
    match v {
        Value::String(s) => Ok(s),
        Value::Object(o) => match vm.heap.obj(o).class {
            Class::String(s) => Ok(s),
            _ => vm.type_error("String.prototype.valueOf called on incompatible receiver"),
        },
        _ => vm.type_error("String.prototype.valueOf called on incompatible receiver"),
    }
}

fn substring_value(vm: &mut Vm, s: StrId, start: usize, end: usize) -> Value {
    let u = vm.heap.units(s).slice(start, end).to_vec();
    vm.str_from_units(u)
}

fn is_regexp(vm: &mut Vm, v: Value) -> JsResult<bool> {
    let Value::Object(o) = v else {
        return Ok(false);
    };
    let key = PropKey::Sym(vm.well_known.match_);
    let m = vm.get(o, key)?;
    if !m.is_undefined() {
        return Ok(vm.to_boolean(m));
    }
    Ok(matches!(vm.heap.obj(o).class, Class::RegExp(_)))
}

// ---- constructor and statics -----------------------------------------------------------

fn string_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = if a.is_empty() {
        vm.names.empty
    } else {
        let v = a.arg(0);
        if !a.is_construct() {
            if let Value::Symbol(sym) = v {
                return Ok(super::symbol::symbol_descriptive_string(vm, sym));
            }
        }
        vm.to_string(v)?
    };
    if !a.is_construct() {
        return Ok(Value::String(s));
    }
    let proto = vm.get_prototype_from_constructor(a.new_target, Intrinsic::StringPrototype)?;
    Ok(Value::Object(vm.string_object(s, proto)))
}

fn string_from_char_code(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let mut out = Vec::with_capacity(a.len());
    for &v in a.args {
        let n = vm.to_number(v)?;
        out.push(crate::convert::f64_to_uint16(n));
    }
    if out.len() == 1 {
        return Ok(Value::String(vm.heap.char_str(out[0])));
    }
    Ok(vm.str_from_units(out))
}

fn string_from_code_point(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let mut b = StrBuilder::with_capacity(a.len());
    for &v in a.args {
        let n = vm.to_number(v)?;
        if n.trunc() != n || !(0.0..=1114111.0).contains(&n) {
            let s = agnijs_js_parser::number::to_js_string(n);
            return vm.range_error(&format!("Invalid code point {s}"));
        }
        b.push_code_point(n as u32);
    }
    Ok(vm.string_from_builder(b))
}

fn string_raw(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let cooked = vm.to_object(a.arg(0))?;
    let raw_key = PropKey::Str(vm.names.raw);
    let raw_v = vm.get(cooked, raw_key)?;
    let raw = vm.to_object(raw_v)?;
    let len = vm.length_of_array_like(raw)?;
    if len <= 0.0 {
        return Ok(Value::String(vm.names.empty));
    }
    let mut b = StrBuilder::new();
    let mut i = 0.0;
    loop {
        let key = vm.number_key(i);
        let seg = vm.get(raw, key)?;
        let seg = vm.to_string(seg)?;
        let u = vm.heap.units(seg);
        b.push_units(&u);
        if i + 1.0 == len {
            break;
        }
        if (i as usize + 1) < a.len() {
            let sub = vm.to_string(a.args[i as usize + 1])?;
            let u = vm.heap.units(sub);
            b.push_units(&u);
        }
        i += 1.0;
    }
    Ok(vm.string_from_builder(b))
}

// ---- prototype -----------------------------------------------------------------------

fn proto_at(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = this_str(vm, a, "at")?;
    let len = vm.heap.str_len(s) as f64;
    let rel = vm.to_integer_or_infinity(a.arg(0))?;
    let k = if rel >= 0.0 { rel } else { len + rel };
    if k < 0.0 || k >= len {
        return Ok(Value::Undefined);
    }
    Ok(substring_value(vm, s, k as usize, k as usize + 1))
}

fn proto_char_at(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = this_str(vm, a, "charAt")?;
    let pos = vm.to_integer_or_infinity(a.arg(0))?;
    let len = vm.heap.str_len(s) as f64;
    if pos < 0.0 || pos >= len {
        return Ok(Value::String(vm.names.empty));
    }
    let unit = vm.heap.units(s).at(pos as usize);
    Ok(Value::String(vm.heap.char_str(unit)))
}

fn proto_char_code_at(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = this_str(vm, a, "charCodeAt")?;
    let pos = vm.to_integer_or_infinity(a.arg(0))?;
    let len = vm.heap.str_len(s) as f64;
    if pos < 0.0 || pos >= len {
        return Ok(Value::Number(f64::NAN));
    }
    let u = vm.heap.units(s).at(pos as usize);
    Ok(Value::Number(u as f64))
}

fn proto_code_point_at(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = this_str(vm, a, "codePointAt")?;
    let pos = vm.to_integer_or_infinity(a.arg(0))?;
    let len = vm.heap.str_len(s) as f64;
    if pos < 0.0 || pos >= len {
        return Ok(Value::Undefined);
    }
    let (cp, _) = vm.heap.units(s).code_point_at(pos as usize);
    Ok(Value::Number(cp as f64))
}

fn proto_concat(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let mut s = this_str(vm, a, "concat")?;
    for &v in a.args {
        let r = vm.to_string(v)?;
        s = vm.heap.concat(s, r);
    }
    Ok(Value::String(s))
}

fn proto_ends_with(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = this_str(vm, a, "endsWith")?;
    if is_regexp(vm, a.arg(0))? {
        return vm.type_error(
            "First argument to String.prototype.endsWith must not be a regular expression",
        );
    }
    let search = vm.to_string(a.arg(0))?;
    let len = vm.heap.str_len(s);
    let end = if a.arg(1).is_undefined() {
        len
    } else {
        vm.to_integer_or_infinity(a.arg(1))?.clamp(0.0, len as f64) as usize
    };
    let search_len = vm.heap.str_len(search);
    if search_len > end {
        return Ok(Value::Bool(false));
    }
    let start = end - search_len;
    let su = units_of(vm, search);
    let hay = vm.heap.units(s);
    Ok(Value::Bool(hay.slice(start, end).eq_units(&su)))
}

fn proto_starts_with(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = this_str(vm, a, "startsWith")?;
    if is_regexp(vm, a.arg(0))? {
        return vm.type_error(
            "First argument to String.prototype.startsWith must not be a regular expression",
        );
    }
    let search = vm.to_string(a.arg(0))?;
    let len = vm.heap.str_len(s);
    let start = if a.arg(1).is_undefined() {
        0
    } else {
        vm.to_integer_or_infinity(a.arg(1))?.clamp(0.0, len as f64) as usize
    };
    let search_len = vm.heap.str_len(search);
    if search_len + start > len {
        return Ok(Value::Bool(false));
    }
    let su = units_of(vm, search);
    let hay = vm.heap.units(s);
    Ok(Value::Bool(
        hay.slice(start, start + search_len).eq_units(&su),
    ))
}

fn proto_includes(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = this_str(vm, a, "includes")?;
    if is_regexp(vm, a.arg(0))? {
        return vm.type_error(
            "First argument to String.prototype.includes must not be a regular expression",
        );
    }
    let search = vm.to_string(a.arg(0))?;
    let len = vm.heap.str_len(s);
    let pos = vm.to_integer_or_infinity(a.arg(1))?.clamp(0.0, len as f64) as usize;
    let su = units_of(vm, search);
    vm.heap.flatten(s);
    let hay = vm.heap.units_flat(s);
    Ok(Value::Bool(hay.index_of(&Units::Two(&su), pos).is_some()))
}

fn proto_index_of(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = this_str(vm, a, "indexOf")?;
    let search = vm.to_string(a.arg(0))?;
    let len = vm.heap.str_len(s);
    let pos = vm.to_integer_or_infinity(a.arg(1))?.clamp(0.0, len as f64) as usize;
    let su = units_of(vm, search);
    vm.heap.flatten(s);
    let hay = vm.heap.units_flat(s);
    Ok(Value::Number(match hay.index_of(&Units::Two(&su), pos) {
        Some(i) => i as f64,
        None => -1.0,
    }))
}

fn proto_last_index_of(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = this_str(vm, a, "lastIndexOf")?;
    let search = vm.to_string(a.arg(0))?;
    let num_pos = vm.to_number(a.arg(1))?;
    let pos = if num_pos.is_nan() {
        f64::INFINITY
    } else {
        to_integer_or_infinity_f64(num_pos)
    };
    let len = vm.heap.str_len(s);
    let search_len = vm.heap.str_len(search);
    let start = pos.clamp(0.0, len as f64) as usize;
    if search_len > len {
        return Ok(Value::Number(-1.0));
    }
    let from = start.min(len - search_len);
    let su = units_of(vm, search);
    vm.heap.flatten(s);
    let hay = vm.heap.units_flat(s);
    Ok(Value::Number(
        match hay.last_index_of(&Units::Two(&su), from) {
            Some(i) => i as f64,
            None => -1.0,
        },
    ))
}

fn proto_is_well_formed(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = this_str(vm, a, "isWellFormed")?;
    Ok(Value::Bool(vm.heap.units(s).is_well_formed()))
}

fn proto_to_well_formed(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = this_str(vm, a, "toWellFormed")?;
    let u = units_of(vm, s);
    let mut out = Vec::with_capacity(u.len());
    let mut i = 0;
    while i < u.len() {
        let c = u[i];
        if (0xD800..=0xDBFF).contains(&c)
            && i + 1 < u.len()
            && (0xDC00..=0xDFFF).contains(&u[i + 1])
        {
            out.push(c);
            out.push(u[i + 1]);
            i += 2;
            continue;
        }
        if (0xD800..=0xDFFF).contains(&c) {
            out.push(0xFFFD);
        } else {
            out.push(c);
        }
        i += 1;
    }
    Ok(vm.str_from_units(out))
}

fn proto_locale_compare(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = this_str(vm, a, "localeCompare")?;
    let that = vm.to_string(a.arg(0))?;
    return super::intl::collator::locale_compare(vm, Value::String(s), Value::String(that), a.arg(1), a.arg(2));
    #[allow(unreachable_code)]
    // Compare by code point after NFC normalisation would be ideal; we
    // compare canonical forms by code unit, which is a consistent total order.
    let su = units_of(vm, s);
    let tu = units_of(vm, that);
    let su = normalize_units(&su, "NFC");
    let tu = normalize_units(&tu, "NFC");
    let a_cp: Vec<u32> = code_points(&su);
    let b_cp: Vec<u32> = code_points(&tu);
    Ok(Value::Number(match a_cp.cmp(&b_cp) {
        std::cmp::Ordering::Less => -1.0,
        std::cmp::Ordering::Equal => 0.0,
        std::cmp::Ordering::Greater => 1.0,
    }))
}

fn code_points(u: &[u16]) -> Vec<u32> {
    let units = Units::Two(u);
    let mut out = Vec::with_capacity(u.len());
    let mut i = 0;
    while i < u.len() {
        let (cp, n) = units.code_point_at(i);
        out.push(cp);
        i += n;
    }
    out
}

fn proto_match(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if a.this.is_nullish() {
        return vm.type_error("String.prototype.match called on null or undefined");
    }
    let regexp = a.arg(0);
    if regexp.is_object() {
        let key = PropKey::Sym(vm.well_known.match_);
        if let Some(matcher) = vm.get_method(regexp, key)? {
            return vm.call_value(matcher, regexp, &[a.this]);
        }
    }
    let s = vm.to_string(a.this)?;
    let rx = super::regexp::regexp_create_from_value(vm, regexp, Value::Undefined)?;
    let key = PropKey::Sym(vm.well_known.match_);
    vm.invoke(rx, key, &[Value::String(s)])
}

fn proto_match_all(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if a.this.is_nullish() {
        return vm.type_error("String.prototype.matchAll called on null or undefined");
    }
    let regexp = a.arg(0);
    if regexp.is_object() {
        if is_regexp(vm, regexp)? {
            let Value::Object(ro) = regexp else {
                unreachable!()
            };
            let flags_key = PropKey::Str(vm.names.flags);
            let flags = vm.get(ro, flags_key)?;
            if flags.is_nullish() {
                return vm.type_error(
                    "String.prototype.matchAll called with a non-global RegExp argument",
                );
            }
            let fs = vm.to_string(flags)?;
            let fu = units_of(vm, fs);
            if !fu.contains(&(b'g' as u16)) {
                return vm.type_error(
                    "String.prototype.matchAll called with a non-global RegExp argument",
                );
            }
        }
        let key = PropKey::Sym(vm.well_known.match_all);
        if let Some(matcher) = vm.get_method(regexp, key)? {
            return vm.call_value(matcher, regexp, &[a.this]);
        }
    }
    let s = vm.to_string(a.this)?;
    let g = vm.str("g");
    let rx = super::regexp::regexp_create_from_value(vm, regexp, g)?;
    let key = PropKey::Sym(vm.well_known.match_all);
    vm.invoke(rx, key, &[Value::String(s)])
}

fn proto_search(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if a.this.is_nullish() {
        return vm.type_error("String.prototype.search called on null or undefined");
    }
    let regexp = a.arg(0);
    if regexp.is_object() {
        let key = PropKey::Sym(vm.well_known.search);
        if let Some(searcher) = vm.get_method(regexp, key)? {
            return vm.call_value(searcher, regexp, &[a.this]);
        }
    }
    let s = vm.to_string(a.this)?;
    let rx = super::regexp::regexp_create_from_value(vm, regexp, Value::Undefined)?;
    let key = PropKey::Sym(vm.well_known.search);
    vm.invoke(rx, key, &[Value::String(s)])
}

/// Unicode normalisation of UTF-16 code units; lone surrogates pass through.
pub fn normalize_units(u: &[u16], form: &str) -> Vec<u16> {
    use icu_normalizer::{ComposingNormalizerBorrowed, DecomposingNormalizerBorrowed};
    let text: String = char::decode_utf16(u.iter().copied())
        .map(|r| r.unwrap_or('\u{FFFD}'))
        .collect();
    // Lone surrogates cannot be represented in a Rust string; map them
    // through placeholders and restore afterwards.
    let has_lone =
        u.iter().any(|&c| (0xD800..=0xDFFF).contains(&c)) && !Units::Two(u).is_well_formed();
    if has_lone {
        // Normalise the well-formed runs separately.
        let mut out = Vec::with_capacity(u.len());
        let mut run = Vec::new();
        let mut i = 0;
        while i < u.len() {
            let c = u[i];
            let pair = (0xD800..=0xDBFF).contains(&c)
                && i + 1 < u.len()
                && (0xDC00..=0xDFFF).contains(&u[i + 1]);
            if pair {
                run.push(c);
                run.push(u[i + 1]);
                i += 2;
            } else if (0xD800..=0xDFFF).contains(&c) {
                if !run.is_empty() {
                    out.extend(normalize_units(&run, form));
                    run.clear();
                }
                out.push(c);
                i += 1;
            } else {
                run.push(c);
                i += 1;
            }
        }
        if !run.is_empty() {
            out.extend(normalize_units(&run, form));
        }
        return out;
    }
    let normalized: String = match form {
        "NFC" => ComposingNormalizerBorrowed::new_nfc()
            .normalize(&text)
            .into_owned(),
        "NFKC" => ComposingNormalizerBorrowed::new_nfkc()
            .normalize(&text)
            .into_owned(),
        "NFD" => DecomposingNormalizerBorrowed::new_nfd()
            .normalize(&text)
            .into_owned(),
        _ => DecomposingNormalizerBorrowed::new_nfkd()
            .normalize(&text)
            .into_owned(),
    };
    normalized.encode_utf16().collect()
}

fn proto_normalize(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = this_str(vm, a, "normalize")?;
    let form = if a.arg(0).is_undefined() {
        "NFC".to_string()
    } else {
        let f = vm.to_string(a.arg(0))?;
        vm.heap.str_to_string(f)
    };
    if !matches!(form.as_str(), "NFC" | "NFD" | "NFKC" | "NFKD") {
        return vm.range_error("The normalization form should be one of NFC, NFD, NFKC, NFKD.");
    }
    let u = units_of(vm, s);
    let out = normalize_units(&u, &form);
    Ok(vm.str_from_units(out))
}

fn pad(vm: &mut Vm, a: &CallArgs, at_start: bool) -> JsResult<Value> {
    let s = this_str(vm, a, if at_start { "padStart" } else { "padEnd" })?;
    let max_len = vm.to_length(a.arg(0))?;
    let len = vm.heap.str_len(s);
    if max_len <= len as f64 {
        return Ok(Value::String(s));
    }
    let filler: Vec<u16> = if a.arg(1).is_undefined() {
        vec![0x20]
    } else {
        let f = vm.to_string(a.arg(1))?;
        units_of(vm, f)
    };
    if filler.is_empty() {
        return Ok(Value::String(s));
    }
    if max_len > (1u64 << 30) as f64 {
        return vm.range_error("Invalid string length");
    }
    let fill_len = max_len as usize - len;
    let mut fill = Vec::with_capacity(fill_len);
    while fill.len() < fill_len {
        let take = (fill_len - fill.len()).min(filler.len());
        fill.extend_from_slice(&filler[..take]);
    }
    let su = units_of(vm, s);
    let out = if at_start {
        let mut v = fill;
        v.extend_from_slice(&su);
        v
    } else {
        let mut v = su;
        v.extend_from_slice(&fill);
        v
    };
    Ok(vm.str_from_units(out))
}

fn proto_pad_end(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    pad(vm, a, false)
}

fn proto_pad_start(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    pad(vm, a, true)
}

fn proto_repeat(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = this_str(vm, a, "repeat")?;
    let n = vm.to_integer_or_infinity(a.arg(0))?;
    if n < 0.0 || n == f64::INFINITY {
        return vm.range_error("Invalid count value");
    }
    let len = vm.heap.str_len(s);
    if n == 0.0 || len == 0 {
        return Ok(Value::String(vm.names.empty));
    }
    if (len as f64) * n > (1u64 << 30) as f64 {
        return vm.range_error("Invalid string length");
    }
    let u = units_of(vm, s);
    let mut out = Vec::with_capacity(len * n as usize);
    for _ in 0..n as usize {
        out.extend_from_slice(&u);
    }
    Ok(vm.str_from_units(out))
}

/// GetSubstitution(matched, str, position, captures, namedCaptures, replacementTemplate).
pub fn get_substitution(
    vm: &mut Vm,
    matched: &[u16],
    s: &[u16],
    position: usize,
    captures: &[Value],
    named: Value,
    template: &[u16],
) -> JsResult<Vec<u16>> {
    let mut result = Vec::with_capacity(template.len());
    let m = captures.len();
    let tail_pos = (position + matched.len()).min(s.len());
    let mut i = 0;
    while i < template.len() {
        let c = template[i];
        if c != b'$' as u16 || i + 1 >= template.len() {
            result.push(c);
            i += 1;
            continue;
        }
        let n = template[i + 1];
        match n {
            0x24 => {
                result.push(0x24);
                i += 2;
            }
            0x26 => {
                result.extend_from_slice(matched);
                i += 2;
            }
            0x60 => {
                result.extend_from_slice(&s[..position.min(s.len())]);
                i += 2;
            }
            0x27 => {
                result.extend_from_slice(&s[tail_pos..]);
                i += 2;
            }
            0x30..=0x39 => {
                let d1 = (n - 0x30) as usize;
                let two = if i + 2 < template.len() && (0x30..=0x39).contains(&template[i + 2]) {
                    Some(d1 * 10 + (template[i + 2] - 0x30) as usize)
                } else {
                    None
                };
                let (index, consumed) = match two {
                    Some(t) if t >= 1 && t <= m => (t, 3),
                    _ if d1 >= 1 && d1 <= m => (d1, 2),
                    _ => {
                        result.push(0x24);
                        i += 1;
                        continue;
                    }
                };
                let cap = captures[index - 1];
                if !cap.is_undefined() {
                    let cs = vm.to_string(cap)?;
                    let cu = vm.heap.units(cs);
                    result.extend(cu.iter());
                }
                i += consumed;
            }
            0x3C => {
                if named.is_undefined() {
                    result.push(0x24);
                    result.push(0x3C);
                    i += 2;
                } else {
                    let rest = &template[i + 2..];
                    match rest.iter().position(|&u| u == 0x3E) {
                        None => {
                            result.push(0x24);
                            result.push(0x3C);
                            i += 2;
                        }
                        Some(gt) => {
                            let group_name = &rest[..gt];
                            let key_str = vm.heap.alloc_str_utf16(group_name.to_vec());
                            let key = vm.key_from_str(key_str);
                            let capture = vm.get_value(named, key)?;
                            if !capture.is_undefined() {
                                let cs = vm.to_string(capture)?;
                                let cu = vm.heap.units(cs);
                                result.extend(cu.iter());
                            }
                            i += 2 + gt + 1;
                        }
                    }
                }
            }
            _ => {
                result.push(0x24);
                i += 1;
            }
        }
    }
    Ok(result)
}

fn proto_replace(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if a.this.is_nullish() {
        return vm.type_error("String.prototype.replace called on null or undefined");
    }
    let search_value = a.arg(0);
    let replace_value = a.arg(1);
    if search_value.is_object() {
        let key = PropKey::Sym(vm.well_known.replace);
        if let Some(replacer) = vm.get_method(search_value, key)? {
            return vm.call_value(replacer, search_value, &[a.this, replace_value]);
        }
    }
    let s = vm.to_string(a.this)?;
    let search = vm.to_string(search_value)?;
    let functional = vm.is_callable(replace_value);
    let replace_str = if functional {
        None
    } else {
        Some(vm.to_string(replace_value)?)
    };
    let su = units_of(vm, s);
    let searchu = units_of(vm, search);
    let Some(pos) = Units::Two(&su).index_of(&Units::Two(&searchu), 0) else {
        return Ok(Value::String(s));
    };
    let replacement: Vec<u16> = if functional {
        let m = vm.str_from_units(searchu.clone());
        let r = vm.call_value(
            replace_value,
            Value::Undefined,
            &[m, Value::Number(pos as f64), Value::String(s)],
        )?;
        let rs = vm.to_string(r)?;
        units_of(vm, rs)
    } else {
        let t = units_of(vm, replace_str.unwrap());
        get_substitution(vm, &searchu, &su, pos, &[], Value::Undefined, &t)?
    };
    let mut out = Vec::with_capacity(su.len() + replacement.len());
    out.extend_from_slice(&su[..pos]);
    out.extend_from_slice(&replacement);
    out.extend_from_slice(&su[pos + searchu.len()..]);
    Ok(vm.str_from_units(out))
}

fn proto_replace_all(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if a.this.is_nullish() {
        return vm.type_error("String.prototype.replaceAll called on null or undefined");
    }
    let search_value = a.arg(0);
    let replace_value = a.arg(1);
    if search_value.is_object() {
        if is_regexp(vm, search_value)? {
            let Value::Object(ro) = search_value else {
                unreachable!()
            };
            let flags_key = PropKey::Str(vm.names.flags);
            let flags = vm.get(ro, flags_key)?;
            if flags.is_nullish() {
                return vm.type_error("replaceAll must be called with a global RegExp");
            }
            let fs = vm.to_string(flags)?;
            let fu = units_of(vm, fs);
            if !fu.contains(&(b'g' as u16)) {
                return vm.type_error("replaceAll must be called with a global RegExp");
            }
        }
        let key = PropKey::Sym(vm.well_known.replace);
        if let Some(replacer) = vm.get_method(search_value, key)? {
            return vm.call_value(replacer, search_value, &[a.this, replace_value]);
        }
    }
    let s = vm.to_string(a.this)?;
    let search = vm.to_string(search_value)?;
    let functional = vm.is_callable(replace_value);
    let replace_str = if functional {
        None
    } else {
        Some(vm.to_string(replace_value)?)
    };
    let su = units_of(vm, s);
    let searchu = units_of(vm, search);
    let search_len = searchu.len();
    let advance = search_len.max(1);
    let mut positions = Vec::new();
    let hay = Units::Two(&su);
    let needle = Units::Two(&searchu);
    let mut pos = hay.index_of(&needle, 0);
    while let Some(p) = pos {
        positions.push(p);
        pos = if p + advance > su.len() {
            None
        } else {
            hay.index_of(&needle, p + advance)
        };
    }
    let template = replace_str.map(|r| units_of(vm, r));
    let mut end_of_last = 0;
    let mut out = Vec::with_capacity(su.len());
    for p in positions {
        let preserved = &su[end_of_last..p];
        let replacement: Vec<u16> = if functional {
            let m = vm.str_from_units(searchu.clone());
            let r = vm.call_value(
                replace_value,
                Value::Undefined,
                &[m, Value::Number(p as f64), Value::String(s)],
            )?;
            let rs = vm.to_string(r)?;
            units_of(vm, rs)
        } else {
            get_substitution(
                vm,
                &searchu,
                &su,
                p,
                &[],
                Value::Undefined,
                template.as_ref().unwrap(),
            )?
        };
        out.extend_from_slice(preserved);
        out.extend_from_slice(&replacement);
        end_of_last = p + search_len;
    }
    if end_of_last < su.len() {
        out.extend_from_slice(&su[end_of_last..]);
    }
    Ok(vm.str_from_units(out))
}

fn proto_slice(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = this_str(vm, a, "slice")?;
    let len = vm.heap.str_len(s) as f64;
    let start = vm.to_integer_or_infinity(a.arg(0))?;
    let from = if start < 0.0 {
        (len + start).max(0.0)
    } else {
        start.min(len)
    };
    let end = if a.arg(1).is_undefined() {
        len
    } else {
        vm.to_integer_or_infinity(a.arg(1))?
    };
    let to = if end < 0.0 {
        (len + end).max(0.0)
    } else {
        end.min(len)
    };
    if from >= to {
        return Ok(Value::String(vm.names.empty));
    }
    Ok(substring_value(vm, s, from as usize, to as usize))
}

fn proto_split(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if a.this.is_nullish() {
        return vm.type_error("String.prototype.split called on null or undefined");
    }
    let separator = a.arg(0);
    let limit = a.arg(1);
    if separator.is_object() {
        let key = PropKey::Sym(vm.well_known.split);
        if let Some(splitter) = vm.get_method(separator, key)? {
            return vm.call_value(splitter, separator, &[a.this, limit]);
        }
    }
    let s = vm.to_string(a.this)?;
    let lim: u32 = if limit.is_undefined() {
        u32::MAX
    } else {
        vm.to_uint32(limit)?
    };
    let r = vm.to_string(separator)?;
    if lim == 0 {
        return Ok(Value::Object(vm.new_array(Vec::new())));
    }
    if separator.is_undefined() {
        return Ok(Value::Object(vm.new_array(vec![Value::String(s)])));
    }
    let su = units_of(vm, s);
    let ru = units_of(vm, r);
    if su.is_empty() {
        if !ru.is_empty() {
            return Ok(Value::Object(vm.new_array(vec![Value::String(s)])));
        }
        return Ok(Value::Object(vm.new_array(Vec::new())));
    }
    let mut parts = Vec::new();
    if ru.is_empty() {
        for &c in su.iter().take(lim as usize) {
            parts.push(vm.str_from_units(vec![c]));
        }
        return Ok(Value::Object(vm.new_array(parts)));
    }
    let hay = Units::Two(&su);
    let needle = Units::Two(&ru);
    let mut i = 0;
    let mut j = hay.index_of(&needle, 0);
    while let Some(jj) = j {
        let piece = vm.str_from_units(su[i..jj].to_vec());
        parts.push(piece);
        if parts.len() as u32 >= lim {
            return Ok(Value::Object(vm.new_array(parts)));
        }
        i = jj + ru.len();
        j = hay.index_of(&needle, i);
    }
    let piece = vm.str_from_units(su[i..].to_vec());
    parts.push(piece);
    Ok(Value::Object(vm.new_array(parts)))
}

fn proto_substring(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = this_str(vm, a, "substring")?;
    let len = vm.heap.str_len(s) as f64;
    let start = vm.to_integer_or_infinity(a.arg(0))?;
    let end = if a.arg(1).is_undefined() {
        len
    } else {
        vm.to_integer_or_infinity(a.arg(1))?
    };
    let fs = start.clamp(0.0, len);
    let fe = end.clamp(0.0, len);
    let (from, to) = if fs < fe { (fs, fe) } else { (fe, fs) };
    Ok(substring_value(vm, s, from as usize, to as usize))
}

fn proto_substr(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = this_str(vm, a, "substr")?;
    let size = vm.heap.str_len(s) as f64;
    let start = vm.to_integer_or_infinity(a.arg(0))?;
    let start = if start == f64::NEG_INFINITY {
        0.0
    } else if start < 0.0 {
        (size + start).max(0.0)
    } else {
        start.min(size)
    };
    let length = if a.arg(1).is_undefined() {
        size
    } else {
        vm.to_integer_or_infinity(a.arg(1))?
    };
    let end = (start + length).min(size);
    if start >= end {
        return Ok(Value::String(vm.names.empty));
    }
    Ok(substring_value(vm, s, start as usize, end as usize))
}

/// Full Unicode case mapping of UTF-16 units, with the final-sigma rule.
pub fn case_map(u: &[u16], upper: bool) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::with_capacity(u.len());
    let units = Units::Two(u);
    let mut i = 0;
    while i < u.len() {
        let (cp, n) = units.code_point_at(i);
        let Some(ch) = char::from_u32(cp) else {
            out.push(cp as u16);
            i += n;
            continue;
        };
        if upper {
            for c in ch.to_uppercase() {
                push_char(&mut out, c);
            }
        } else if ch == '\u{03A3}' && is_final_sigma(&units, i) {
            out.push(0x03C2);
        } else {
            for c in ch.to_lowercase() {
                push_char(&mut out, c);
            }
        }
        i += n;
    }
    out
}

fn push_char(out: &mut Vec<u16>, c: char) {
    let mut buf = [0u16; 2];
    out.extend_from_slice(c.encode_utf16(&mut buf));
}

fn is_case_ignorable(c: char) -> bool {
    // Mn, Me, Cf, Lm, Sk plus the listed Word_Break values; approximated by
    // the common members.
    matches!(
        c,
        '\'' | '.'
            | ':'
            | '^'
            | '`'
            | '\u{00A8}'
            | '\u{00AD}'
            | '\u{00AF}'
            | '\u{00B4}'
            | '\u{00B7}'
            | '\u{00B8}'
            | '\u{2018}'
            | '\u{2019}'
            | '\u{2024}'
            | '\u{2027}'
    ) || ('\u{02B0}'..='\u{02FF}').contains(&c)
        || ('\u{0300}'..='\u{036F}').contains(&c)
        || ('\u{0600}'..='\u{0605}').contains(&c)
        || ('\u{0610}'..='\u{061A}').contains(&c)
        || ('\u{064B}'..='\u{065F}').contains(&c)
        || ('\u{06D6}'..='\u{06DD}').contains(&c)
        || ('\u{180B}'..='\u{180F}').contains(&c)
        || ('\u{1AB0}'..='\u{1AFF}').contains(&c)
        || ('\u{1D2C}'..='\u{1D6A}').contains(&c)
        || ('\u{1DC0}'..='\u{1DFF}').contains(&c)
        || ('\u{20D0}'..='\u{20F0}').contains(&c)
        || ('\u{FE20}'..='\u{FE2F}').contains(&c)
        || ('\u{1D167}'..='\u{1D169}').contains(&c)
        || ('\u{1D17B}'..='\u{1D182}').contains(&c)
        || ('\u{1D185}'..='\u{1D18B}').contains(&c)
        || ('\u{1D1AA}'..='\u{1D1AD}').contains(&c)
        || ('\u{1D242}'..='\u{1D244}').contains(&c)
        || ('\u{E0001}'..='\u{E01EF}').contains(&c)
        || matches!(c, '\u{061C}' | '\u{0640}' | '\u{0670}' | '\u{070F}' | '\u{FEFF}' | '\u{0374}' | '\u{037A}' | '\u{0559}')
        || ('\u{0483}'..='\u{0489}').contains(&c)
        || ('\u{0591}'..='\u{05BD}').contains(&c)
        || ('\u{200B}'..='\u{200F}').contains(&c)
        || ('\u{202A}'..='\u{202E}').contains(&c)
        || ('\u{2060}'..='\u{2064}').contains(&c)
        || ('\u{FE00}'..='\u{FE0F}').contains(&c)
        || ('\u{FE20}'..='\u{FE2F}').contains(&c)
        || c == '\u{FEFF}'
        || ('\u{02B0}'..='\u{02FF}').contains(&c)
        || ('\u{1D2C}'..='\u{1D6A}').contains(&c)
}

fn is_cased(c: char) -> bool {
    c.is_lowercase()
        || c.is_uppercase()
        || ('\u{01C5}'..='\u{01CB}').contains(&c)
        || c == '\u{01F2}'
        || ('\u{1F88}'..='\u{1FAF}').contains(&c)
        || ('\u{1FBC}'..='\u{1FFC}').contains(&c)
}

/// Final_Sigma: preceded by a cased letter (skipping case-ignorables) and
/// not followed by one.
fn is_final_sigma(units: &Units<'_>, i: usize) -> bool {
    let len = units.len();
    // Look backwards.
    let mut j = i;
    let mut before = false;
    while j > 0 {
        let mut k = j - 1;
        if k > 0
            && (0xDC00..=0xDFFF).contains(&units.at(k))
            && (0xD800..=0xDBFF).contains(&units.at(k - 1))
        {
            k -= 1;
        }
        let (cp, _) = units.code_point_at(k);
        let Some(c) = char::from_u32(cp) else { break };
        if is_case_ignorable(c) {
            j = k;
            continue;
        }
        before = is_cased(c);
        break;
    }
    if !before {
        return false;
    }
    let mut j = i + 1;
    while j < len {
        let (cp, n) = units.code_point_at(j);
        let Some(c) = char::from_u32(cp) else {
            return true;
        };
        if is_case_ignorable(c) {
            j += n;
            continue;
        }
        return !is_cased(c);
    }
    true
}

fn proto_to_lower_case(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = this_str(vm, a, "toLowerCase")?;
    let u = units_of(vm, s);
    let out = case_map(&u, false);
    Ok(vm.str_from_units(out))
}

/// ECMA-402 TransformCase: the requested locale's case mapping (the host
/// default locale when none is given).
fn locale_case(vm: &mut Vm, a: &CallArgs, name: &str, upper: bool) -> JsResult<Value> {
    let s = this_str(vm, a, name)?;
    let requested = super::intl::abstract_ops::canonicalize_locale_list(vm, a.arg(0))?;
    let tag = match requested.first() {
        Some(t) => t.clone(),
        None => super::intl::abstract_ops::default_locale(vm),
    };
    let u = units_of(vm, s);
    let out = agnijs_js_intl::casemap::map_utf16(&u, &tag, upper);
    Ok(vm.str_from_units(out))
}

fn proto_to_locale_lower_case(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    locale_case(vm, a, "toLocaleLowerCase", false)
}

fn proto_to_locale_upper_case(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    locale_case(vm, a, "toLocaleUpperCase", true)
}

fn proto_to_upper_case(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = this_str(vm, a, "toUpperCase")?;
    let u = units_of(vm, s);
    let out = case_map(&u, true);
    Ok(vm.str_from_units(out))
}

fn proto_value_of(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    Ok(Value::String(this_string_value(vm, a.this)?))
}

fn trim_impl(vm: &mut Vm, a: &CallArgs, start: bool, end: bool, name: &str) -> JsResult<Value> {
    let s = this_str(vm, a, name)?;
    let out = vm.trim_units(s, start, end);
    Ok(vm.str_from_units(out))
}

fn proto_trim(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    trim_impl(vm, a, true, true, "trim")
}

fn proto_trim_start(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    trim_impl(vm, a, true, false, "trimStart")
}

fn proto_trim_end(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    trim_impl(vm, a, false, true, "trimEnd")
}

fn proto_iterator(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = this_str(vm, a, "[Symbol.iterator]")?;
    Ok(create_string_iterator(vm, s))
}

// ---- Annex B HTML methods ---------------------------------------------------------------

fn create_html(vm: &mut Vm, a: &CallArgs, tag: &str, attribute: Option<&str>) -> JsResult<Value> {
    if a.this.is_nullish() {
        return vm.type_error(&format!(
            "String.prototype.{tag} called on null or undefined"
        ));
    }
    let s = vm.to_string(a.this)?;
    let mut b = StrBuilder::new();
    b.push_str(&format!("<{tag}"));
    if let Some(attr) = attribute {
        let v = vm.to_string(a.arg(0))?;
        let vu = units_of(vm, v);
        b.push_str(&format!(" {attr}=\""));
        for &c in &vu {
            if c == 0x22 {
                b.push_str("&quot;");
            } else {
                b.push(c);
            }
        }
        b.push(0x22);
    }
    b.push(0x3E);
    let su = vm.heap.units(s);
    b.push_units(&su);
    b.push_str(&format!("</{tag}>"));
    Ok(vm.string_from_builder(b))
}

fn html_anchor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    create_html(vm, a, "a", Some("name"))
}
fn html_big(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    create_html(vm, a, "big", None)
}
fn html_blink(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    create_html(vm, a, "blink", None)
}
fn html_bold(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    create_html(vm, a, "b", None)
}
fn html_fixed(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    create_html(vm, a, "tt", None)
}
fn html_fontcolor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    create_html(vm, a, "font", Some("color"))
}
fn html_fontsize(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    create_html(vm, a, "font", Some("size"))
}
fn html_italics(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    create_html(vm, a, "i", None)
}
fn html_link(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    create_html(vm, a, "a", Some("href"))
}
fn html_small(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    create_html(vm, a, "small", None)
}
fn html_strike(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    create_html(vm, a, "strike", None)
}
fn html_sub(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    create_html(vm, a, "sub", None)
}
fn html_sup(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    create_html(vm, a, "sup", None)
}

#[allow(dead_code)]
fn _ws(u: u16) -> bool {
    is_js_whitespace(u)
}

#[allow(dead_code)]
fn _obj(_o: ObjId) {}
