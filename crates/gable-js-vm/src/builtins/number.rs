//! Number constructor, Number.prototype and exact decimal formatting.

use crate::object::{CallArgs, Class, ObjectData};
use crate::realm::Intrinsic;
use crate::value::Value;
use crate::vm::{JsResult, Vm};
use gable_engine_api::RealmId;
use num_bigint::BigInt;
use num_traits::{One, Zero};

pub fn install(vm: &mut Vm, realm: RealmId) {
    let object_proto = vm.intrinsic(Intrinsic::ObjectPrototype);
    let proto = vm.alloc(ObjectData::new(Some(object_proto), Class::Number(0.0)));
    let ctor = super::install_ctor(
        vm,
        realm,
        "Number",
        1,
        number_constructor,
        proto,
        Intrinsic::Number,
        Intrinsic::NumberPrototype,
    );

    vm.constant(ctor, "EPSILON", Value::Number(f64::EPSILON));
    vm.constant(ctor, "MAX_SAFE_INTEGER", Value::Number(9007199254740991.0));
    vm.constant(ctor, "MAX_VALUE", Value::Number(f64::MAX));
    vm.constant(ctor, "MIN_SAFE_INTEGER", Value::Number(-9007199254740991.0));
    vm.constant(ctor, "MIN_VALUE", Value::Number(5e-324));
    vm.constant(ctor, "NaN", Value::Number(f64::NAN));
    vm.constant(ctor, "NEGATIVE_INFINITY", Value::Number(f64::NEG_INFINITY));
    vm.constant(ctor, "POSITIVE_INFINITY", Value::Number(f64::INFINITY));
    vm.method(ctor, "isFinite", 1, number_is_finite);
    vm.method(ctor, "isInteger", 1, number_is_integer);
    vm.method(ctor, "isNaN", 1, number_is_nan);
    vm.method(ctor, "isSafeInteger", 1, number_is_safe_integer);

    vm.method(proto, "toExponential", 1, proto_to_exponential);
    vm.method(proto, "toFixed", 1, proto_to_fixed);
    vm.method(proto, "toLocaleString", 0, proto_to_locale_string);
    vm.method(proto, "toPrecision", 1, proto_to_precision);
    vm.method(proto, "toString", 1, proto_to_string);
    vm.method(proto, "valueOf", 0, proto_value_of);
}

fn number_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let n = if a.is_empty() {
        0.0
    } else {
        let prim = vm.to_numeric(a.arg(0))?;
        match prim {
            Value::BigInt(b) => crate::bigint::to_f64(&vm.heap.bigint(b).0),
            Value::Number(n) => n,
            _ => unreachable!(),
        }
    };
    if !a.is_construct() {
        return Ok(Value::Number(n));
    }
    let o = vm.ordinary_create_from_constructor(
        a.new_target,
        Intrinsic::NumberPrototype,
        Class::Number(n),
    )?;
    Ok(Value::Object(o))
}

fn number_is_finite(_vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    Ok(Value::Bool(
        matches!(a.arg(0), Value::Number(n) if n.is_finite()),
    ))
}

fn number_is_integer(_vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    Ok(Value::Bool(
        matches!(a.arg(0), Value::Number(n) if n.is_finite() && n.trunc() == n),
    ))
}

fn number_is_nan(_vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    Ok(Value::Bool(
        matches!(a.arg(0), Value::Number(n) if n.is_nan()),
    ))
}

fn number_is_safe_integer(_vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    Ok(Value::Bool(
        matches!(a.arg(0), Value::Number(n) if n.is_finite() && n.trunc() == n && n.abs() <= 9007199254740991.0),
    ))
}

fn this_number_value(vm: &mut Vm, v: Value) -> JsResult<f64> {
    match v {
        Value::Number(n) => Ok(n),
        Value::Object(o) => match vm.heap.obj(o).class {
            Class::Number(n) => Ok(n),
            _ => vm.type_error("Number.prototype method called on incompatible receiver"),
        },
        _ => vm.type_error("Number.prototype method called on incompatible receiver"),
    }
}

fn proto_value_of(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    Ok(Value::Number(this_number_value(vm, a.this)?))
}

fn proto_to_locale_string(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let x = this_number_value(vm, a.this)?;
    Ok(Value::String(vm.number_to_string(x)))
}

fn proto_to_string(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let x = this_number_value(vm, a.this)?;
    let radix = if a.arg(0).is_undefined() {
        10.0
    } else {
        vm.to_integer_or_infinity(a.arg(0))?
    };
    if !(2.0..=36.0).contains(&radix) {
        return vm.range_error("toString() radix must be between 2 and 36");
    }
    if radix == 10.0 {
        return Ok(Value::String(vm.number_to_string(x)));
    }
    Ok(vm.str(&number_to_radix_string(x, radix as u32)))
}

/// Number::toString(x, radix) for radix != 10 (V8's algorithm).
pub fn number_to_radix_string(value: f64, radix: u32) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value < 0.0 {
            "-Infinity".into()
        } else {
            "Infinity".into()
        };
    }
    if value == 0.0 {
        return "0".into();
    }
    const CHARS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let negative = value < 0.0;
    let value = value.abs();
    let r = radix as f64;
    let mut integer = value.floor();
    let mut fraction = value - integer;
    let mut delta = 0.5 * (next_double(value) - value);
    delta = delta.max(next_double(0.0));
    let mut frac_digits: Vec<u8> = Vec::new();
    if fraction >= delta {
        loop {
            fraction *= r;
            delta *= r;
            let digit = fraction as usize;
            frac_digits.push(CHARS[digit]);
            fraction -= digit as f64;
            if fraction > 0.5 || (fraction == 0.5 && (digit & 1) == 1) {
                if fraction + delta > 1.0 {
                    // Round up with carry propagation.
                    loop {
                        match frac_digits.pop() {
                            None => {
                                integer += 1.0;
                                break;
                            }
                            Some(c) => {
                                let d = if c > b'9' {
                                    (c - b'a') as u32 + 10
                                } else {
                                    (c - b'0') as u32
                                };
                                if d + 1 < radix {
                                    frac_digits.push(CHARS[(d + 1) as usize]);
                                    break;
                                }
                            }
                        }
                    }
                    break;
                }
            }
            if fraction < delta {
                break;
            }
        }
    }
    let mut int_digits: Vec<u8> = Vec::new();
    // Fill digits below the double's precision with zeros.
    while (integer / r) >= 9007199254740992.0 {
        integer /= r;
        int_digits.push(b'0');
    }
    loop {
        let remainder = integer % r;
        int_digits.push(CHARS[remainder as usize]);
        integer = (integer - remainder) / r;
        if integer <= 0.0 {
            break;
        }
    }
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    for &c in int_digits.iter().rev() {
        out.push(c as char);
    }
    if !frac_digits.is_empty() {
        out.push('.');
        for &c in &frac_digits {
            out.push(c as char);
        }
    }
    out
}

fn next_double(x: f64) -> f64 {
    if x.is_nan() || x == f64::INFINITY {
        return x;
    }
    if x == 0.0 {
        return f64::from_bits(1);
    }
    let bits = x.to_bits();
    if x > 0.0 {
        f64::from_bits(bits + 1)
    } else {
        f64::from_bits(bits - 1)
    }
}

// ---- exact decimal formatting ----------------------------------------------------------

/// Decompose a finite positive double into (mantissa, exponent) with
/// value = m * 2^e exactly.
fn decompose(x: f64) -> (BigInt, i32) {
    let bits = x.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1u64 << 52) - 1);
    if exp == 0 {
        (BigInt::from(frac), -1074)
    } else {
        (BigInt::from(frac | (1u64 << 52)), exp - 1075)
    }
}

/// x * 10^k as an exact rational p/q (q > 0).
fn scaled(x: f64, k: i32) -> (BigInt, BigInt) {
    let (m, e) = decompose(x);
    let mut p = m;
    let mut q = BigInt::one();
    if e >= 0 {
        p <<= e as usize;
    } else {
        q <<= (-e) as usize;
    }
    if k >= 0 {
        p *= BigInt::from(10u32).pow(k as u32);
    } else {
        q *= BigInt::from(10u32).pow((-k) as u32);
    }
    (p, q)
}

/// round-half-up(p / q).
fn round_div(p: &BigInt, q: &BigInt) -> BigInt {
    (p * 2 + q) / (q * 2)
}

/// Decimal exponent e such that 10^e <= x < 10^(e+1) for finite x > 0.
fn decimal_exponent(x: f64) -> i32 {
    let mut e = x.log10().floor() as i32;
    // Correct the floating estimate with exact comparisons.
    loop {
        let (p, q) = scaled(x, -e);
        if p < q {
            e -= 1;
            continue;
        }
        if p >= &q * 10 {
            e += 1;
            continue;
        }
        return e;
    }
}

/// Number.prototype.toFixed.
fn proto_to_fixed(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let x = this_number_value(vm, a.this)?;
    let f = vm.to_integer_or_infinity(a.arg(0))?;
    if !f.is_finite() || !(0.0..=100.0).contains(&f) {
        return vm.range_error("toFixed() digits argument must be between 0 and 100");
    }
    if !x.is_finite() {
        return Ok(Value::String(vm.number_to_string(x)));
    }
    let f = f as i32;
    if x.abs() >= 1e21 {
        return Ok(Value::String(vm.number_to_string(x)));
    }
    let s = to_fixed_string(x, f);
    Ok(vm.str(&s))
}

pub fn to_fixed_string(x: f64, f: i32) -> String {
    let negative = x < 0.0;
    let ax = x.abs();
    let n = if ax == 0.0 {
        BigInt::zero()
    } else {
        let (p, q) = scaled(ax, f);
        round_div(&p, &q)
    };
    let mut digits = n.to_string();
    if f > 0 {
        while digits.len() <= f as usize {
            digits.insert(0, '0');
        }
        let split = digits.len() - f as usize;
        digits.insert(split, '.');
    }
    if negative {
        format!("-{digits}")
    } else {
        digits
    }
}

/// Digits and exponent of `x` (finite, > 0) with `f + 1` significant digits:
/// returns (digit string of length f+1, e) with n * 10^(e-f) closest to x.
fn exponent_digits(x: f64, f: i32) -> (String, i32) {
    let mut e = decimal_exponent(x);
    let (p, q) = scaled(x, f - e);
    let mut n = round_div(&p, &q);
    let limit = BigInt::from(10u32).pow((f + 1) as u32);
    if n >= limit {
        n /= 10;
        e += 1;
    }
    (n.to_string(), e)
}

/// Shortest round-trip digits of `x` (finite, > 0): (digits, e) with
/// x = 0.d1d2... * 10^(e+1), i.e. d1.d2d3... * 10^e.
fn shortest_digits(x: f64) -> (String, i32) {
    let s = format!("{x:e}");
    let (mant, exp) = s.split_once('e').unwrap();
    let e: i32 = exp.parse().unwrap();
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    (digits, e)
}

fn proto_to_exponential(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let x = this_number_value(vm, a.this)?;
    let fd = a.arg(0);
    let f = vm.to_integer_or_infinity(fd)?;
    if !x.is_finite() {
        return Ok(Value::String(vm.number_to_string(x)));
    }
    if !f.is_finite() || !(0.0..=100.0).contains(&f) {
        return vm.range_error("toExponential() argument must be between 0 and 100");
    }
    let negative = x < 0.0;
    let ax = x.abs();
    let (digits, e) = if ax == 0.0 {
        let count = if fd.is_undefined() { 1 } else { f as usize + 1 };
        ("0".repeat(count), 0)
    } else if fd.is_undefined() {
        shortest_digits(ax)
    } else {
        exponent_digits(ax, f as i32)
    };
    let mut m = String::new();
    m.push_str(&digits[..1]);
    if digits.len() > 1 {
        m.push('.');
        m.push_str(&digits[1..]);
    }
    let sign = if e < 0 { '-' } else { '+' };
    let s = format!("{}{m}e{sign}{}", if negative { "-" } else { "" }, e.abs());
    Ok(vm.str(&s))
}

fn proto_to_precision(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let x = this_number_value(vm, a.this)?;
    if a.arg(0).is_undefined() {
        return Ok(Value::String(vm.number_to_string(x)));
    }
    let p = vm.to_integer_or_infinity(a.arg(0))?;
    if !x.is_finite() {
        return Ok(Value::String(vm.number_to_string(x)));
    }
    if !p.is_finite() || !(1.0..=100.0).contains(&p) {
        return vm.range_error("toPrecision() argument must be between 1 and 100");
    }
    let p = p as i32;
    let negative = x < 0.0;
    let ax = x.abs();
    let (digits, e) = if ax == 0.0 {
        ("0".repeat(p as usize), 0)
    } else {
        exponent_digits(ax, p - 1)
    };
    let mut s = String::new();
    if negative {
        s.push('-');
    }
    if e < -6 || e >= p {
        s.push_str(&digits[..1]);
        if p > 1 {
            s.push('.');
            s.push_str(&digits[1..]);
        }
        s.push('e');
        s.push(if e < 0 { '-' } else { '+' });
        s.push_str(&e.abs().to_string());
    } else if e == p - 1 {
        s.push_str(&digits);
    } else if e >= 0 {
        s.push_str(&digits[..(e + 1) as usize]);
        s.push('.');
        s.push_str(&digits[(e + 1) as usize..]);
    } else {
        s.push_str("0.");
        for _ in 0..(-(e + 1)) {
            s.push('0');
        }
        s.push_str(&digits);
    }
    Ok(vm.str(&s))
}
