//! Uint8Array to and from base64 and hex (ES2026): `toBase64`, `toHex`,
//! `setFromBase64`, `setFromHex` on %Uint8Array.prototype% and
//! `fromBase64`, `fromHex` on %Uint8Array%.

use crate::builtins::typedarray::{ta_length, ElementKind};
use crate::builtins::typedarray_ctor::{allocate_typed_array, kind_intrinsics, ta_data};
use crate::object::{CallArgs, Class};
use crate::value::{ObjId, PropKey, Value};
use crate::vm::{JsResult, Vm};
use agnijs_engine_api::RealmId;

pub fn install(vm: &mut Vm, _realm: RealmId) {
    let (ci, pi) = kind_intrinsics(ElementKind::Uint8);
    let ctor = vm.intrinsic(ci);
    let proto = vm.intrinsic(pi);
    vm.method(ctor, "fromBase64", 1, from_base64);
    vm.method(ctor, "fromHex", 1, from_hex);
    vm.method(proto, "setFromBase64", 1, set_from_base64);
    vm.method(proto, "setFromHex", 1, set_from_hex);
    vm.method(proto, "toBase64", 0, to_base64);
    vm.method(proto, "toHex", 0, to_hex);
}

const STD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

#[derive(Clone, Copy, PartialEq, Eq)]
enum LastChunk {
    Loose,
    Strict,
    StopBeforePartial,
}

/// ValidateUint8Array.
fn validate_uint8(vm: &mut Vm, v: Value, method: &str) -> JsResult<ObjId> {
    if let Value::Object(o) = v {
        if ta_data(vm, o).map(|t| t.kind) == Some(ElementKind::Uint8) {
            return Ok(o);
        }
    }
    vm.type_error(&format!("Uint8Array.prototype.{method} called on a value that is not a Uint8Array"))
}

/// The bytes a Uint8Array views (GetUint8ArrayBytes): its offset and
/// length into the buffer, or a TypeError when detached or out of bounds.
fn view(vm: &mut Vm, o: ObjId) -> JsResult<(ObjId, usize, usize)> {
    let Some(len) = ta_length(vm, o) else {
        return vm.type_error("the Uint8Array is detached or out of bounds");
    };
    let t = ta_data(vm, o).expect("validated typed array");
    Ok((t.buffer, t.byte_offset, len))
}

fn buffer_bytes(vm: &mut Vm, buffer: ObjId) -> &mut Vec<u8> {
    match &mut vm.heap.obj_mut(buffer).class {
        Class::ArrayBuffer(b) => &mut b.bytes,
        _ => unreachable!("a typed array's buffer is an ArrayBuffer"),
    }
}

/// GetOptionsObject: undefined is no options; anything else but an object
/// is a TypeError.
fn options_object(vm: &mut Vm, v: Value) -> JsResult<Option<ObjId>> {
    match v {
        Value::Undefined => Ok(None),
        Value::Object(o) => Ok(Some(o)),
        _ => vm.type_error("options must be an object"),
    }
}

fn get_option(vm: &mut Vm, opts: Option<ObjId>, name: &str) -> JsResult<Value> {
    match opts {
        None => Ok(Value::Undefined),
        Some(o) => {
            let key = PropKey::Str(vm.atom(name));
            vm.get(o, key)
        }
    }
}

/// The `alphabet` option: `true` for base64url.
fn alphabet_option(vm: &mut Vm, opts: Option<ObjId>) -> JsResult<bool> {
    match get_option(vm, opts, "alphabet")? {
        Value::Undefined => Ok(false),
        Value::String(s) if vm.heap.str_eq_ascii(s, "base64") => Ok(false),
        Value::String(s) if vm.heap.str_eq_ascii(s, "base64url") => Ok(true),
        _ => vm.type_error("alphabet must be \"base64\" or \"base64url\""),
    }
}

fn last_chunk_option(vm: &mut Vm, opts: Option<ObjId>) -> JsResult<LastChunk> {
    match get_option(vm, opts, "lastChunkHandling")? {
        Value::Undefined => Ok(LastChunk::Loose),
        Value::String(s) if vm.heap.str_eq_ascii(s, "loose") => Ok(LastChunk::Loose),
        Value::String(s) if vm.heap.str_eq_ascii(s, "strict") => Ok(LastChunk::Strict),
        Value::String(s) if vm.heap.str_eq_ascii(s, "stop-before-partial") => Ok(LastChunk::StopBeforePartial),
        _ => vm.type_error("lastChunkHandling must be \"loose\", \"strict\" or \"stop-before-partial\""),
    }
}

fn string_arg(vm: &mut Vm, v: Value, what: &str) -> JsResult<Vec<u16>> {
    let Value::String(s) = v else {
        return vm.type_error(&format!("{what}: the argument must be a string"));
    };
    Ok(vm.heap.units(s).iter().collect())
}

// ---- encoding -----------------------------------------------------------------------------

fn to_base64(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = validate_uint8(vm, a.this, "toBase64")?;
    let opts = options_object(vm, a.arg(0))?;
    let url = alphabet_option(vm, opts)?;
    let omit_padding = get_option(vm, opts, "omitPadding")?;
    let omit_padding = vm.to_boolean(omit_padding);
    let (buffer, off, len) = view(vm, o)?;
    // The length is known before the string is built (TASK-103.54).
    let out_len = if omit_padding {
        len.saturating_mul(4).div_ceil(3)
    } else {
        len.div_ceil(3).saturating_mul(4)
    };
    vm.check_string_len(out_len)?;
    let table = if url { URL } else { STD };
    let bytes = &buffer_bytes(vm, buffer)[off..off + len];
    let mut out = Vec::with_capacity(out_len);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        let chars = [(n >> 18) & 63, (n >> 12) & 63, (n >> 6) & 63, n & 63];
        let keep = chunk.len() + 1;
        for (i, &c) in chars.iter().enumerate() {
            if i < keep {
                out.push(table[c as usize]);
            } else if !omit_padding {
                out.push(b'=');
            }
        }
    }
    Ok(Value::String(vm.heap.alloc_str_latin1(out)))
}

fn to_hex(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = validate_uint8(vm, a.this, "toHex")?;
    let (buffer, off, len) = view(vm, o)?;
    // The length is known before the string is built (TASK-103.54).
    vm.check_string_len(len.saturating_mul(2))?;
    let bytes = &buffer_bytes(vm, buffer)[off..off + len];
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = Vec::with_capacity(len * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize]);
        out.push(HEX[(b & 15) as usize]);
    }
    Ok(Value::String(vm.heap.alloc_str_latin1(out)))
}

// ---- decoding -----------------------------------------------------------------------------

/// The outcome of FromBase64 / FromHex: the input consumed by complete
/// chunks, the bytes decoded and whether decoding stopped on a SyntaxError
/// (the bytes before it are still written by the `setFrom` methods).
struct Decoded {
    read: usize,
    bytes: Vec<u8>,
    error: bool,
}

fn is_ascii_ws(c: u16) -> bool {
    matches!(c, 0x09 | 0x0A | 0x0C | 0x0D | 0x20)
}

fn skip_ws(s: &[u16], mut i: usize) -> usize {
    while i < s.len() && is_ascii_ws(s[i]) {
        i += 1;
    }
    i
}

fn b64_value(c: u16) -> Option<u32> {
    let c = u8::try_from(c).ok()?;
    STD.iter().position(|&x| x == c).map(|p| p as u32)
}

/// DecodeBase64Chunk: `None` when `strict_bits` and the padding bits of a
/// short chunk are not zero.
fn decode_chunk(chunk: &[u32], strict_bits: bool, out: &mut Vec<u8>) -> bool {
    let n = chunk.len();
    let mut v = 0u32;
    for i in 0..4 {
        v = v << 6 | chunk.get(i).copied().unwrap_or(0);
    }
    let b = [(v >> 16) as u8, (v >> 8) as u8, v as u8];
    match n {
        2 => {
            if strict_bits && b[1] != 0 {
                return false;
            }
            out.push(b[0]);
        }
        3 => {
            if strict_bits && b[2] != 0 {
                return false;
            }
            out.extend_from_slice(&b[..2]);
        }
        _ => out.extend_from_slice(&b),
    }
    true
}

/// FromBase64(string, alphabet, lastChunkHandling, maxLength).
fn decode_base64(s: &[u16], url: bool, last: LastChunk, max_len: usize) -> Decoded {
    let mut d = Decoded { read: 0, bytes: Vec::new(), error: false };
    if max_len == 0 {
        return d;
    }
    let mut chunk: Vec<u32> = Vec::with_capacity(4);
    let mut index = 0;
    let len = s.len();
    loop {
        index = skip_ws(s, index);
        if index == len {
            if !chunk.is_empty() {
                match last {
                    LastChunk::StopBeforePartial => return d,
                    LastChunk::Loose => {
                        if chunk.len() == 1 {
                            d.error = true;
                            return d;
                        }
                        decode_chunk(&chunk, false, &mut d.bytes);
                    }
                    LastChunk::Strict => {
                        d.error = true;
                        return d;
                    }
                }
            }
            d.read = len;
            return d;
        }
        let mut c = s[index];
        index += 1;
        if c == u16::from(b'=') {
            if chunk.len() < 2 {
                d.error = true;
                return d;
            }
            index = skip_ws(s, index);
            if chunk.len() == 2 {
                if index == len {
                    if last == LastChunk::StopBeforePartial {
                        return d;
                    }
                    d.error = true;
                    return d;
                }
                if s[index] == u16::from(b'=') {
                    index = skip_ws(s, index + 1);
                }
            }
            if index < len {
                d.error = true;
                return d;
            }
            if !decode_chunk(&chunk, last == LastChunk::Strict, &mut d.bytes) {
                d.error = true;
                return d;
            }
            d.read = len;
            return d;
        }
        if url {
            if c == u16::from(b'+') || c == u16::from(b'/') {
                d.error = true;
                return d;
            }
            if c == u16::from(b'-') {
                c = u16::from(b'+');
            } else if c == u16::from(b'_') {
                c = u16::from(b'/');
            }
        }
        let Some(v) = b64_value(c) else {
            d.error = true;
            return d;
        };
        let remaining = max_len - d.bytes.len();
        if (remaining == 1 && chunk.len() == 2) || (remaining == 2 && chunk.len() == 3) {
            return d;
        }
        chunk.push(v);
        if chunk.len() == 4 {
            decode_chunk(&chunk, false, &mut d.bytes);
            chunk.clear();
            d.read = index;
            if d.bytes.len() == max_len {
                return d;
            }
        }
    }
}

fn hex_value(c: u16) -> Option<u8> {
    char::from_u32(c as u32).and_then(|ch| ch.to_digit(16)).map(|v| v as u8)
}

/// FromHex(string, maxLength).
fn decode_hex(s: &[u16], max_len: usize) -> Decoded {
    let mut d = Decoded { read: 0, bytes: Vec::new(), error: false };
    if s.len() % 2 != 0 {
        d.error = true;
        return d;
    }
    while d.read < s.len() && d.bytes.len() < max_len {
        let (Some(hi), Some(lo)) = (hex_value(s[d.read]), hex_value(s[d.read + 1])) else {
            d.error = true;
            return d;
        };
        d.read += 2;
        d.bytes.push(hi << 4 | lo);
    }
    d
}

fn new_uint8_array(vm: &mut Vm, bytes: Vec<u8>) -> JsResult<Value> {
    let (_, pi) = kind_intrinsics(ElementKind::Uint8);
    let proto = vm.intrinsic(pi);
    let o = allocate_typed_array(vm, ElementKind::Uint8, proto, bytes.len() as f64)?;
    let (buffer, off, len) = view(vm, o)?;
    buffer_bytes(vm, buffer)[off..off + len].copy_from_slice(&bytes);
    Ok(Value::Object(o))
}

fn from_base64(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = string_arg(vm, a.arg(0), "Uint8Array.fromBase64")?;
    let opts = options_object(vm, a.arg(1))?;
    let url = alphabet_option(vm, opts)?;
    let last = last_chunk_option(vm, opts)?;
    let d = decode_base64(&s, url, last, usize::MAX);
    if d.error {
        return Err(vm.throw_syntax("Uint8Array.fromBase64: invalid base64 string"));
    }
    new_uint8_array(vm, d.bytes)
}

fn from_hex(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = string_arg(vm, a.arg(0), "Uint8Array.fromHex")?;
    let d = decode_hex(&s, usize::MAX);
    if d.error {
        return Err(vm.throw_syntax("Uint8Array.fromHex: invalid hex string"));
    }
    new_uint8_array(vm, d.bytes)
}

/// Write the decoded bytes into the array, then throw if decoding stopped
/// on an error, else return `{ read, written }`.
fn set_result(vm: &mut Vm, o: ObjId, d: Decoded, what: &str) -> JsResult<Value> {
    let (buffer, off, _) = view(vm, o)?;
    let n = d.bytes.len();
    buffer_bytes(vm, buffer)[off..off + n].copy_from_slice(&d.bytes);
    if d.error {
        return Err(vm.throw_syntax(&format!("Uint8Array.prototype.{what}: invalid input")));
    }
    let r = vm.new_object();
    let read = PropKey::Str(vm.atom("read"));
    let written = PropKey::Str(vm.atom("written"));
    vm.create_data_property_or_throw(r, read, Value::Number(d.read as f64))?;
    vm.create_data_property_or_throw(r, written, Value::Number(n as f64))?;
    Ok(Value::Object(r))
}

fn set_from_base64(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = validate_uint8(vm, a.this, "setFromBase64")?;
    let s = string_arg(vm, a.arg(0), "Uint8Array.prototype.setFromBase64")?;
    let opts = options_object(vm, a.arg(1))?;
    let url = alphabet_option(vm, opts)?;
    let last = last_chunk_option(vm, opts)?;
    let (_, _, len) = view(vm, o)?;
    let d = decode_base64(&s, url, last, len);
    set_result(vm, o, d, "setFromBase64")
}

fn set_from_hex(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = validate_uint8(vm, a.this, "setFromHex")?;
    let s = string_arg(vm, a.arg(0), "Uint8Array.prototype.setFromHex")?;
    let (_, _, len) = view(vm, o)?;
    let d = decode_hex(&s, len);
    set_result(vm, o, d, "setFromHex")
}
