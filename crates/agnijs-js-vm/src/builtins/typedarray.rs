//! ArrayBuffer, SharedArrayBuffer, typed arrays and DataView: the data model
//! and the integer-indexed exotic hooks. The constructors live in
//! `arraybuffer.rs`, `typedarray_ctor.rs` and `atomics.rs`.

use crate::gc::Tracer;
use crate::object::Class;
use crate::value::{Attrs, ObjId, PropKey, Property, PropertyDescriptor, Value};
use crate::vm::{JsResult, Vm};

pub struct BufferData {
    pub bytes: Vec<u8>,
    pub detached: bool,
    pub shared: bool,
    /// `Some(max)` for resizable buffers.
    pub max_byte_length: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ElementKind {
    Int8,
    Uint8,
    Uint8Clamped,
    Int16,
    Uint16,
    Int32,
    Uint32,
    Float16,
    Float32,
    Float64,
    BigInt64,
    BigUint64,
}

impl ElementKind {
    pub fn size(self) -> usize {
        match self {
            ElementKind::Int8 | ElementKind::Uint8 | ElementKind::Uint8Clamped => 1,
            ElementKind::Int16 | ElementKind::Uint16 | ElementKind::Float16 => 2,
            ElementKind::Int32 | ElementKind::Uint32 | ElementKind::Float32 => 4,
            ElementKind::Float64 | ElementKind::BigInt64 | ElementKind::BigUint64 => 8,
        }
    }
    pub fn is_bigint(self) -> bool {
        matches!(self, ElementKind::BigInt64 | ElementKind::BigUint64)
    }
}

pub struct TypedArrayData {
    pub buffer: ObjId,
    pub kind: ElementKind,
    pub byte_offset: usize,
    /// `None` for length-tracking views on resizable buffers.
    pub length: Option<usize>,
}

impl TypedArrayData {
    pub fn trace(&self, t: &mut Tracer) {
        t.obj(self.buffer);
    }
}

pub struct DataViewData {
    pub buffer: ObjId,
    pub byte_offset: usize,
    pub byte_length: Option<usize>,
}

/// IsTypedArrayFixedLength: not length-tracking and not over a resizable
/// (or growable) buffer.
pub fn is_fixed_length(vm: &Vm, o: ObjId) -> bool {
    match &vm.heap.obj(o).class {
        Class::TypedArray(t) => {
            t.length.is_some()
                && !matches!(&vm.heap.obj(t.buffer).class, Class::ArrayBuffer(b) if b.max_byte_length.is_some())
        }
        _ => true,
    }
}

/// True when the view's buffer is detached.
pub fn is_detached_view(vm: &Vm, o: ObjId) -> bool {
    let buffer = match &vm.heap.obj(o).class {
        Class::TypedArray(t) => t.buffer,
        Class::DataView(d) => d.buffer,
        _ => return false,
    };
    matches!(&vm.heap.obj(buffer).class, Class::ArrayBuffer(b) if b.detached)
}

/// CanonicalNumericIndexString: `Some(n)` for keys that address elements
/// (including out-of-range and `-0`), `None` for ordinary keys.
pub fn canonical_numeric_index(vm: &mut Vm, key: PropKey) -> Option<f64> {
    match key {
        PropKey::Index(i) => Some(i as f64),
        PropKey::Str(s) => {
            let units = vm.heap.string(s).units();
            if units.eq_str("-0") {
                return Some(-0.0);
            }
            let text = units.to_string_lossy();
            let n = crate::convert::parse_js_number(&text);
            let back = agnijs_js_parser::number::to_js_string(n);
            if back == text {
                Some(n)
            } else {
                None
            }
        }
        PropKey::Sym(_) => None,
    }
}

/// TypedArrayLength, or `None` when out of bounds / detached.
pub fn ta_length(vm: &Vm, o: ObjId) -> Option<usize> {
    let Class::TypedArray(t) = &vm.heap.obj(o).class else {
        return None;
    };
    let Class::ArrayBuffer(b) = &vm.heap.obj(t.buffer).class else {
        return None;
    };
    if b.detached {
        return None;
    }
    match t.length {
        Some(n) => {
            if t.byte_offset + n * t.kind.size() > b.bytes.len() {
                None
            } else {
                Some(n)
            }
        }
        None => {
            if t.byte_offset > b.bytes.len() {
                None
            } else {
                Some((b.bytes.len() - t.byte_offset) / t.kind.size())
            }
        }
    }
}

/// IsValidIntegerIndex.
pub fn valid_index(vm: &Vm, o: ObjId, n: f64) -> Option<usize> {
    if n != n.trunc() || n.is_infinite() || (n == 0.0 && n.is_sign_negative()) || n < 0.0 {
        return None;
    }
    let len = ta_length(vm, o)?;
    if n >= len as f64 {
        return None;
    }
    Some(n as usize)
}

pub fn ta_get_own_property(vm: &mut Vm, o: ObjId, key: PropKey) -> JsResult<Option<Property>> {
    match canonical_numeric_index(vm, key) {
        None => Ok(vm.heap.get_own_property(o, key)),
        Some(n) => match valid_index(vm, o, n) {
            None => Ok(None),
            Some(i) => {
                let v = ta_get_element(vm, o, i);
                Ok(Some(Property::data(v, Attrs::ALL)))
            }
        },
    }
}

pub fn ta_has_property(vm: &mut Vm, o: ObjId, key: PropKey) -> JsResult<Option<bool>> {
    match canonical_numeric_index(vm, key) {
        None => Ok(None),
        Some(n) => Ok(Some(valid_index(vm, o, n).is_some())),
    }
}

pub fn ta_define_own_property(
    vm: &mut Vm,
    o: ObjId,
    key: PropKey,
    desc: PropertyDescriptor,
) -> JsResult<bool> {
    match canonical_numeric_index(vm, key) {
        None => Ok(vm.ordinary_define_own_property(o, key, desc)),
        Some(n) => {
            let Some(i) = valid_index(vm, o, n) else {
                return Ok(false);
            };
            if desc.configurable == Some(false)
                || desc.enumerable == Some(false)
                || desc.is_accessor()
                || desc.writable == Some(false)
            {
                return Ok(false);
            }
            if let Some(v) = desc.value {
                ta_set_element(vm, o, i, v)?;
            }
            Ok(true)
        }
    }
}

pub fn ta_get(vm: &mut Vm, o: ObjId, key: PropKey) -> JsResult<Option<Value>> {
    match canonical_numeric_index(vm, key) {
        None => Ok(None),
        Some(n) => match valid_index(vm, o, n) {
            None => Ok(Some(Value::Undefined)),
            Some(i) => Ok(Some(ta_get_element(vm, o, i))),
        },
    }
}

pub fn ta_set(
    vm: &mut Vm,
    o: ObjId,
    key: PropKey,
    v: Value,
    receiver: Value,
) -> JsResult<Option<bool>> {
    match canonical_numeric_index(vm, key) {
        None => Ok(None),
        Some(n) => {
            if receiver == Value::Object(o) {
                // TypedArraySetElement: convert first, then check the index.
                let converted = ta_convert(vm, o, v)?;
                if let Some(i) = valid_index(vm, o, n) {
                    ta_store(vm, o, i, converted);
                }
                return Ok(Some(true));
            }
            if valid_index(vm, o, n).is_none() {
                return Ok(Some(true));
            }
            Ok(None)
        }
    }
}

pub fn ta_delete(vm: &mut Vm, o: ObjId, key: PropKey) -> JsResult<Option<bool>> {
    match canonical_numeric_index(vm, key) {
        None => Ok(None),
        Some(n) => Ok(Some(valid_index(vm, o, n).is_none())),
    }
}

pub fn ta_own_keys(vm: &mut Vm, o: ObjId) -> JsResult<Vec<PropKey>> {
    let len = ta_length(vm, o).unwrap_or(0);
    let mut keys: Vec<PropKey> = (0..len as u32).map(PropKey::Index).collect();
    let rest = vm.heap.own_keys(o);
    keys.extend(rest.into_iter().filter(|k| !matches!(k, PropKey::Index(_))));
    Ok(keys)
}

/// A converted element ready to store.
#[derive(Clone, Copy)]
pub enum Converted {
    Num(f64),
    Big(i128),
}

pub fn ta_convert(vm: &mut Vm, o: ObjId, v: Value) -> JsResult<Converted> {
    let kind = match &vm.heap.obj(o).class {
        Class::TypedArray(t) => t.kind,
        _ => unreachable!(),
    };
    if kind.is_bigint() {
        let b = vm.to_bigint(v)?;
        Ok(Converted::Big(crate::bigint::to_i128_wrapping(
            &vm.heap.bigint(b).0,
        )))
    } else {
        Ok(Converted::Num(vm.to_number(v)?))
    }
}

pub fn ta_set_element(vm: &mut Vm, o: ObjId, i: usize, v: Value) -> JsResult<()> {
    let c = ta_convert(vm, o, v)?;
    if valid_index(vm, o, i as f64).is_some() {
        ta_store(vm, o, i, c);
    }
    Ok(())
}

pub fn ta_store(vm: &mut Vm, o: ObjId, i: usize, c: Converted) {
    let (buffer, kind, offset) = match &vm.heap.obj(o).class {
        Class::TypedArray(t) => (t.buffer, t.kind, t.byte_offset),
        _ => unreachable!(),
    };
    let Class::ArrayBuffer(b) = &mut vm.heap.obj_mut(buffer).class else {
        unreachable!()
    };
    let at = offset + i * kind.size();
    write_element(&mut b.bytes[at..at + kind.size()], kind, c);
}

pub fn ta_get_element(vm: &mut Vm, o: ObjId, i: usize) -> Value {
    let (buffer, kind, offset) = match &vm.heap.obj(o).class {
        Class::TypedArray(t) => (t.buffer, t.kind, t.byte_offset),
        _ => unreachable!(),
    };
    let raw = {
        let Class::ArrayBuffer(b) = &vm.heap.obj(buffer).class else {
            unreachable!()
        };
        let at = offset + i * kind.size();
        read_element(&b.bytes[at..at + kind.size()], kind)
    };
    match raw {
        Converted::Num(n) => Value::Number(n),
        Converted::Big(b) => vm.bigint_value(num_bigint::BigInt::from(b)),
    }
}

pub fn write_element(dst: &mut [u8], kind: ElementKind, c: Converted) {
    let n = match c {
        Converted::Num(n) => n,
        Converted::Big(b) => {
            match kind {
                ElementKind::BigInt64 => dst.copy_from_slice(&(b as i64).to_le_bytes()),
                ElementKind::BigUint64 => dst.copy_from_slice(&(b as u64).to_le_bytes()),
                _ => unreachable!(),
            }
            return;
        }
    };
    match kind {
        ElementKind::Int8 => dst[0] = crate::convert::f64_to_int32(n) as i8 as u8,
        ElementKind::Uint8 => dst[0] = crate::convert::f64_to_int32(n) as u8,
        ElementKind::Uint8Clamped => dst[0] = clamp_u8(n),
        ElementKind::Int16 => {
            dst.copy_from_slice(&(crate::convert::f64_to_int32(n) as i16).to_le_bytes())
        }
        ElementKind::Uint16 => {
            dst.copy_from_slice(&(crate::convert::f64_to_int32(n) as u16).to_le_bytes())
        }
        ElementKind::Int32 => dst.copy_from_slice(&crate::convert::f64_to_int32(n).to_le_bytes()),
        ElementKind::Uint32 => dst.copy_from_slice(&crate::convert::f64_to_uint32(n).to_le_bytes()),
        ElementKind::Float16 => dst.copy_from_slice(&f64_to_f16_bits(n).to_le_bytes()),
        // NaN is stored as the one quiet NaN, whatever bits arithmetic gave
        // it (x86 and ARM differ), so buffer bytes are the same on every
        // platform (TASK-103.1, docs/determinism.md). The spec leaves the
        // pattern to the implementation.
        ElementKind::Float32 => {
            let f = if n.is_nan() { f32::from_bits(0x7fc0_0000) } else { n as f32 };
            dst.copy_from_slice(&f.to_le_bytes())
        }
        ElementKind::Float64 => {
            let f = if n.is_nan() { f64::from_bits(0x7ff8_0000_0000_0000) } else { n };
            dst.copy_from_slice(&f.to_le_bytes())
        }
        ElementKind::BigInt64 | ElementKind::BigUint64 => unreachable!(),
    }
}

/// ToUint8Clamp.
pub fn clamp_u8(n: f64) -> u8 {
    if n.is_nan() || n <= 0.0 {
        0
    } else if n >= 255.0 {
        255
    } else {
        let f = n.floor();
        let d = n - f;
        let r = if d < 0.5 {
            f
        } else if d > 0.5 {
            f + 1.0
        } else if f % 2.0 == 0.0 {
            f
        } else {
            f + 1.0
        };
        r as u8
    }
}

pub fn read_element(src: &[u8], kind: ElementKind) -> Converted {
    match kind {
        ElementKind::Int8 => Converted::Num(src[0] as i8 as f64),
        ElementKind::Uint8 | ElementKind::Uint8Clamped => Converted::Num(src[0] as f64),
        ElementKind::Int16 => Converted::Num(i16::from_le_bytes([src[0], src[1]]) as f64),
        ElementKind::Uint16 => Converted::Num(u16::from_le_bytes([src[0], src[1]]) as f64),
        ElementKind::Int32 => {
            Converted::Num(i32::from_le_bytes(src[..4].try_into().unwrap()) as f64)
        }
        ElementKind::Uint32 => {
            Converted::Num(u32::from_le_bytes(src[..4].try_into().unwrap()) as f64)
        }
        ElementKind::Float16 => {
            Converted::Num(f16_bits_to_f64(u16::from_le_bytes([src[0], src[1]])))
        }
        ElementKind::Float32 => {
            Converted::Num(f32::from_le_bytes(src[..4].try_into().unwrap()) as f64)
        }
        ElementKind::Float64 => Converted::Num(f64::from_le_bytes(src[..8].try_into().unwrap())),
        ElementKind::BigInt64 => {
            Converted::Big(i64::from_le_bytes(src[..8].try_into().unwrap()) as i128)
        }
        ElementKind::BigUint64 => {
            Converted::Big(u64::from_le_bytes(src[..8].try_into().unwrap()) as i128)
        }
    }
}

/// IEEE 754 binary16 conversion with round-half-to-even.
pub fn f64_to_f16_bits(v: f64) -> u16 {
    if v.is_nan() {
        return 0x7e00;
    }
    let sign: u16 = if v.is_sign_negative() { 0x8000 } else { 0 };
    let a = v.abs();
    if a.is_infinite() {
        return sign | 0x7c00;
    }
    if a == 0.0 {
        return sign;
    }
    // Largest finite half is 65504; values at or above 65520 round to infinity.
    if a >= 65520.0 {
        return sign | 0x7c00;
    }
    let bits = a.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32 - 1023;
    if exp < -25 {
        return sign;
    }
    if exp >= -14 {
        let mant = bits & ((1u64 << 52) - 1);
        let shift = 52 - 10;
        let mut m = (mant >> shift) as u32;
        let rem = mant & ((1u64 << shift) - 1);
        let half = 1u64 << (shift - 1);
        if rem > half || (rem == half && (m & 1) == 1) {
            m += 1;
        }
        let mut e = (exp + 15) as u32;
        if m == 0x400 {
            m = 0;
            e += 1;
        }
        if e >= 31 {
            return sign | 0x7c00;
        }
        return sign | ((e as u16) << 10) | (m as u16);
    }
    // Subnormal half: value = m * 2^-24.
    let scaled = a * (1u64 << 24) as f64;
    let mut m = scaled.floor();
    let d = scaled - m;
    if d > 0.5 || (d == 0.5 && (m as u64) % 2 == 1) {
        m += 1.0;
    }
    let m = m as u16;
    if m >= 0x400 {
        return sign | (1 << 10);
    }
    sign | m
}

pub fn f16_bits_to_f64(h: u16) -> f64 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = ((h >> 10) & 0x1f) as i32;
    let mant = (h & 0x3ff) as f64;
    if exp == 0 {
        return sign * mant * 2f64.powi(-24);
    }
    if exp == 31 {
        return if mant == 0.0 {
            sign * f64::INFINITY
        } else {
            f64::NAN
        };
    }
    sign * (1.0 + mant / 1024.0) * 2f64.powi(exp - 15)
}

pub fn install(vm: &mut Vm, realm: agnijs_engine_api::RealmId) {
    super::arraybuffer::install(vm, realm);
    super::typedarray_ctor::install(vm, realm);
    super::atomics::install(vm, realm);
}
