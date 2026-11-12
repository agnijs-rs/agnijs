//! Global object properties: value constants, eval, number parsing, URI
//! handling, Annex B escape/unescape and the host console.

use crate::convert::{parse_js_number, parse_radix_f64};
use crate::object::CallArgs;
use crate::realm::Intrinsic;
use crate::string::{is_js_whitespace, StrBuilder};
use crate::value::{Attrs, Value};
use crate::vm::{ErrorType, JsResult, Vm};
use agnijs_engine_api::{ConsoleLevel, RealmId};

pub fn install(vm: &mut Vm, realm: RealmId) {
    let global = vm.realm_of(realm).global;
    vm.define_raw_str(global, "NaN", Value::Number(f64::NAN), Attrs::NONE);
    vm.define_raw_str(
        global,
        "Infinity",
        Value::Number(f64::INFINITY),
        Attrs::NONE,
    );
    vm.define_raw_str(global, "undefined", Value::Undefined, Attrs::NONE);

    let eval = vm.method(global, "eval", 1, global_eval);
    vm.realm_mut().set_intrinsic(Intrinsic::Eval, eval);
    vm.method(global, "isFinite", 1, global_is_finite);
    vm.method(global, "isNaN", 1, global_is_nan);
    let pf = vm.method(global, "parseFloat", 1, global_parse_float);
    let pi = vm.method(global, "parseInt", 2, global_parse_int);
    vm.method(global, "decodeURI", 1, global_decode_uri);
    vm.method(global, "decodeURIComponent", 1, global_decode_uri_component);
    vm.method(global, "encodeURI", 1, global_encode_uri);
    vm.method(global, "encodeURIComponent", 1, global_encode_uri_component);
    vm.method(global, "escape", 1, global_escape);
    vm.method(global, "unescape", 1, global_unescape);

    // Number.parseFloat / Number.parseInt are the same function objects.
    let number = vm.intrinsic(Intrinsic::Number);
    vm.define_raw_str(number, "parseFloat", Value::Object(pf), Attrs::METHOD);
    vm.define_raw_str(number, "parseInt", Value::Object(pi), Attrs::METHOD);

    let console = vm.new_object();
    vm.method(console, "log", 0, console_log);
    vm.method(console, "info", 0, console_info);
    vm.method(console, "warn", 0, console_warn);
    vm.method(console, "error", 0, console_error);
    vm.method(console, "debug", 0, console_debug);
    vm.define_raw_str(global, "console", Value::Object(console), Attrs::METHOD);
}

fn global_eval(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let realm = vm.current_realm;
    vm.indirect_eval(realm, a.arg(0))
}

fn global_is_finite(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let n = vm.to_number(a.arg(0))?;
    Ok(Value::Bool(n.is_finite()))
}

fn global_is_nan(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let n = vm.to_number(a.arg(0))?;
    Ok(Value::Bool(n.is_nan()))
}

/// parseFloat(string): the longest prefix that is a StrDecimalLiteral.
fn global_parse_float(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = vm.to_string(a.arg(0))?;
    let units = vm.heap.units(s).to_vec();
    let mut i = 0;
    while i < units.len() && is_js_whitespace(units[i]) {
        i += 1;
    }
    let rest: String = units[i..]
        .iter()
        .map(|&u| if u < 128 { u as u8 as char } else { '\u{FFFD}' })
        .collect();
    Ok(Value::Number(parse_float_prefix(&rest)))
}

pub fn parse_float_prefix(s: &str) -> f64 {
    let b = s.as_bytes();
    let mut i = 0;
    let mut neg = false;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        neg = b[i] == b'-';
        i += 1;
    }
    if s[i..].starts_with("Infinity") {
        return if neg {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    let start = i;
    let mut digits = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if i < b.len() && b[i] == b'.' {
        let mut j = i + 1;
        let mut frac = 0;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
            frac += 1;
        }
        if digits + frac > 0 {
            i = j;
            digits += frac;
        }
    }
    if digits == 0 {
        return f64::NAN;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        let exp_start = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > exp_start {
            i = j;
        }
    }
    let n = parse_js_number(&s[start..i]);
    if neg {
        -n
    } else {
        n
    }
}

/// parseInt(string, radix).
fn global_parse_int(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = vm.to_string(a.arg(0))?;
    let radix = vm.to_int32(a.arg(1))?;
    let units = vm.heap.units(s).to_vec();
    Ok(Value::Number(parse_int_units(&units, radix)))
}

pub fn parse_int_units(units: &[u16], radix: i32) -> f64 {
    let mut i = 0;
    while i < units.len() && is_js_whitespace(units[i]) {
        i += 1;
    }
    let mut sign = 1.0;
    if i < units.len() && (units[i] == b'+' as u16 || units[i] == b'-' as u16) {
        if units[i] == b'-' as u16 {
            sign = -1.0;
        }
        i += 1;
    }
    let mut r = radix;
    let mut strip_prefix = true;
    if r != 0 {
        if !(2..=36).contains(&r) {
            return f64::NAN;
        }
        if r != 16 {
            strip_prefix = false;
        }
    } else {
        r = 10;
    }
    if strip_prefix
        && i + 1 < units.len()
        && units[i] == b'0' as u16
        && (units[i + 1] == b'x' as u16 || units[i + 1] == b'X' as u16)
    {
        i += 2;
        r = 16;
    }
    let mut digits = String::new();
    while i < units.len() {
        let u = units[i];
        let d = match u {
            0x30..=0x39 => (u - 0x30) as i32,
            0x61..=0x7a => (u - 0x61) as i32 + 10,
            0x41..=0x5a => (u - 0x41) as i32 + 10,
            _ => break,
        };
        if d >= r {
            break;
        }
        digits.push(u as u8 as char);
        i += 1;
    }
    if digits.is_empty() {
        return f64::NAN;
    }
    let n = if r == 10 {
        parse_js_number(&digits)
    } else {
        parse_radix_f64(&digits, r as u32)
    };
    sign * n
}

// ---- URI handling --------------------------------------------------------------------

const URI_RESERVED: &str = ";/?:@&=+$,";
const URI_UNESCAPED_EXTRA: &str = "-_.!~*'()";

fn is_uri_unescaped(c: u16) -> bool {
    (c < 128)
        && ((c as u8).is_ascii_alphanumeric()
            || URI_UNESCAPED_EXTRA.as_bytes().contains(&(c as u8)))
}

fn is_uri_reserved(c: u16) -> bool {
    c < 128 && URI_RESERVED.as_bytes().contains(&(c as u8))
}

fn uri_error<T>(vm: &mut Vm, msg: &str) -> JsResult<T> {
    Err(vm.throw(ErrorType::URIError, msg))
}

/// Encode(string, extraUnescaped).
fn encode(vm: &mut Vm, s: Value, extra: fn(u16) -> bool) -> JsResult<Value> {
    let s = vm.to_string(s)?;
    let units = vm.heap.units(s).to_vec();
    let mut out = StrBuilder::with_capacity(units.len());
    let mut k = 0;
    while k < units.len() {
        vm.check_string_len(out.len())?;
        let c = units[k];
        if is_uri_unescaped(c) || extra(c) {
            out.push(c);
            k += 1;
            continue;
        }
        let cp: u32 = if (0xDC00..=0xDFFF).contains(&c) {
            return uri_error(vm, "URI malformed");
        } else if (0xD800..=0xDBFF).contains(&c) {
            k += 1;
            if k >= units.len() || !(0xDC00..=0xDFFF).contains(&units[k]) {
                return uri_error(vm, "URI malformed");
            }
            0x10000 + (((c as u32) - 0xD800) << 10) + ((units[k] as u32) - 0xDC00)
        } else {
            c as u32
        };
        let ch = char::from_u32(cp).unwrap();
        let mut buf = [0u8; 4];
        for byte in ch.encode_utf8(&mut buf).bytes() {
            out.push_str(&format!("%{byte:02X}"));
        }
        k += 1;
    }
    vm.string_from_builder(out)
}

fn hex_val(u: u16) -> Option<u32> {
    match u {
        0x30..=0x39 => Some((u - 0x30) as u32),
        0x61..=0x66 => Some((u - 0x61) as u32 + 10),
        0x41..=0x46 => Some((u - 0x41) as u32 + 10),
        _ => None,
    }
}

fn decode_byte(units: &[u16], k: usize) -> Option<u8> {
    if units.get(k)? != &(b'%' as u16) {
        return None;
    }
    let h = hex_val(*units.get(k + 1)?)?;
    let l = hex_val(*units.get(k + 2)?)?;
    Some(((h << 4) | l) as u8)
}

/// Decode(string, preserveEscapeSet).
fn decode(vm: &mut Vm, s: Value, preserve: fn(u16) -> bool) -> JsResult<Value> {
    let s = vm.to_string(s)?;
    let units = vm.heap.units(s).to_vec();
    let mut out = StrBuilder::with_capacity(units.len());
    let mut k = 0;
    while k < units.len() {
        let c = units[k];
        if c != b'%' as u16 {
            out.push(c);
            k += 1;
            continue;
        }
        let start = k;
        let Some(b) = decode_byte(&units, k) else {
            return uri_error(vm, "URI malformed");
        };
        k += 3;
        if b & 0x80 == 0 {
            let ch = b as u16;
            if preserve(ch) {
                out.push_slice(&units[start..k]);
            } else {
                out.push(ch);
            }
            continue;
        }
        let n = if b & 0xE0 == 0xC0 {
            2
        } else if b & 0xF0 == 0xE0 {
            3
        } else if b & 0xF8 == 0xF0 {
            4
        } else {
            return uri_error(vm, "URI malformed");
        };
        let mut bytes = vec![b];
        for _ in 1..n {
            let Some(cb) = decode_byte(&units, k) else {
                return uri_error(vm, "URI malformed");
            };
            if cb & 0xC0 != 0x80 {
                return uri_error(vm, "URI malformed");
            }
            bytes.push(cb);
            k += 3;
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            return uri_error(vm, "URI malformed");
        };
        let ch = text.chars().next().unwrap();
        let cp = ch as u32;
        if cp < 0x10000 {
            let u = cp as u16;
            if preserve(u) {
                out.push_slice(&units[start..k]);
            } else {
                out.push(u);
            }
        } else {
            out.push_code_point(cp);
        }
    }
    vm.string_from_builder(out)
}

fn reserved_plus_hash(c: u16) -> bool {
    is_uri_reserved(c) || c == b'#' as u16
}

fn none(_c: u16) -> bool {
    false
}

fn global_decode_uri(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    decode(vm, a.arg(0), reserved_plus_hash)
}

fn global_decode_uri_component(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    decode(vm, a.arg(0), none)
}

fn global_encode_uri(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    encode(vm, a.arg(0), reserved_plus_hash)
}

fn global_encode_uri_component(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    encode(vm, a.arg(0), none)
}

fn global_escape(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = vm.to_string(a.arg(0))?;
    let units = vm.heap.units(s).to_vec();
    let mut out = StrBuilder::with_capacity(units.len());
    for &c in &units {
        vm.check_string_len(out.len())?;
        if c < 128 && ((c as u8).is_ascii_alphanumeric() || b"@*_+-./".contains(&(c as u8))) {
            out.push(c);
        } else if c < 256 {
            out.push_str(&format!("%{c:02X}"));
        } else {
            out.push_str(&format!("%u{c:04X}"));
        }
    }
    vm.string_from_builder(out)
}

fn global_unescape(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = vm.to_string(a.arg(0))?;
    let units = vm.heap.units(s).to_vec();
    let mut out = StrBuilder::with_capacity(units.len());
    let mut k = 0;
    while k < units.len() {
        let c = units[k];
        if c == b'%' as u16 {
            if k + 6 <= units.len() && units[k + 1] == b'u' as u16 {
                let hex: Option<Vec<u32>> =
                    units[k + 2..k + 6].iter().map(|&u| hex_val(u)).collect();
                if let Some(h) = hex {
                    let v = (h[0] << 12) | (h[1] << 8) | (h[2] << 4) | h[3];
                    out.push(v as u16);
                    k += 6;
                    continue;
                }
            }
            if k + 3 <= units.len() {
                if let (Some(h), Some(l)) = (hex_val(units[k + 1]), hex_val(units[k + 2])) {
                    out.push(((h << 4) | l) as u16);
                    k += 3;
                    continue;
                }
            }
        }
        out.push(c);
        k += 1;
    }
    vm.string_from_builder(out)
}

// ---- console ---------------------------------------------------------------------------

fn console_emit(vm: &mut Vm, a: &CallArgs, level: ConsoleLevel) -> JsResult<Value> {
    if vm.debug_hot {
        vm.debug_console(level, a.args);
    }
    let mut parts = Vec::with_capacity(a.len());
    for &v in a.args {
        let s = match v {
            Value::Symbol(s) => {
                let Value::String(d) = super::symbol::symbol_descriptive_string(vm, s) else {
                    unreachable!()
                };
                vm.heap.str_to_string(d)
            }
            _ => vm.to_rust_string(v)?,
        };
        parts.push(s);
    }
    let host = vm.host.clone();
    host.console(vm.current_realm, level, parts);
    Ok(Value::Undefined)
}

fn console_log(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    console_emit(vm, a, ConsoleLevel::Log)
}

fn console_info(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    console_emit(vm, a, ConsoleLevel::Info)
}

fn console_warn(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    console_emit(vm, a, ConsoleLevel::Warn)
}

fn console_error(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    console_emit(vm, a, ConsoleLevel::Error)
}

fn console_debug(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    console_emit(vm, a, ConsoleLevel::Debug)
}
