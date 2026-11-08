//! Type conversions, equality and the arithmetic operators.

use crate::interp::step::ArithOp;
use crate::object::{Class, ObjectData};
use crate::realm::Intrinsic;
use crate::string::{is_js_whitespace, trim_range, StrBuilder, Units};
use crate::value::{Attrs, BigId, ObjId, PropKey, Property, StrId, Value};
use crate::vm::{JsResult, Vm};
use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive, Zero};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hint {
    Default,
    Number,
    String,
}

/// ToInt32 on a number.
pub fn f64_to_int32(n: f64) -> i32 {
    if n.is_nan() || n.is_infinite() {
        return 0;
    }
    if n.abs() < 2147483648.0 {
        return n as i32;
    }
    let m = n.trunc() % 4294967296.0;
    let m = if m < 0.0 { m + 4294967296.0 } else { m };
    (m as u32) as i32
}

pub fn f64_to_uint32(n: f64) -> u32 {
    f64_to_int32(n) as u32
}

pub fn f64_to_uint16(n: f64) -> u16 {
    f64_to_int32(n) as u16
}

/// ToIntegerOrInfinity on a number.
pub fn to_integer_or_infinity_f64(n: f64) -> f64 {
    if n.is_nan() {
        0.0
    } else if n.is_infinite() {
        n
    } else {
        let t = n.trunc();
        if t == 0.0 {
            0.0
        } else {
            t
        }
    }
}

/// StringToNumber.
pub fn string_to_number(u: &Units<'_>) -> f64 {
    let (start, end) = trim_range(u, true, end_true());
    let s = u.slice(start, end);
    if s.is_empty() {
        return 0.0;
    }
    // Everything legal is ASCII.
    let mut bytes = Vec::with_capacity(s.len());
    for c in s.iter() {
        if c >= 128 {
            return f64::NAN;
        }
        bytes.push(c as u8);
    }
    let text = std::str::from_utf8(&bytes).unwrap();
    parse_js_number(text)
}

#[inline]
fn end_true() -> bool {
    true
}

/// Parse the StringNumericLiteral grammar (already trimmed, ASCII).
pub fn parse_js_number(text: &str) -> f64 {
    let b = text.as_bytes();
    if b.len() > 2 && b[0] == b'0' {
        let radix = match b[1] {
            b'x' | b'X' => 16,
            b'o' | b'O' => 8,
            b'b' | b'B' => 2,
            _ => 0,
        };
        if radix != 0 {
            let digits = &text[2..];
            if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
                return f64::NAN;
            }
            return parse_radix_f64(digits, radix);
        }
    }
    let (neg, rest) = match b.first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    if rest == "Infinity" {
        return if neg {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    // StrUnsignedDecimalLiteral: digits [. digits] [e[+-]digits] | . digits [exp]
    let rb = rest.as_bytes();
    let mut i = 0;
    let mut int_digits = 0;
    while i < rb.len() && rb[i].is_ascii_digit() {
        i += 1;
        int_digits += 1;
    }
    let mut frac_digits = 0;
    if i < rb.len() && rb[i] == b'.' {
        i += 1;
        while i < rb.len() && rb[i].is_ascii_digit() {
            i += 1;
            frac_digits += 1;
        }
    }
    if int_digits == 0 && frac_digits == 0 {
        return f64::NAN;
    }
    if i < rb.len() && (rb[i] == b'e' || rb[i] == b'E') {
        i += 1;
        if i < rb.len() && (rb[i] == b'+' || rb[i] == b'-') {
            i += 1;
        }
        let mut exp_digits = 0;
        while i < rb.len() && rb[i].is_ascii_digit() {
            i += 1;
            exp_digits += 1;
        }
        if exp_digits == 0 {
            return f64::NAN;
        }
    }
    if i != rb.len() {
        return f64::NAN;
    }
    let v: f64 = rest.parse().unwrap_or(f64::NAN);
    if neg {
        -v
    } else {
        v
    }
}

/// Digits in a radix to an f64 with correct rounding for large values.
pub fn parse_radix_f64(digits: &str, radix: u32) -> f64 {
    // Exact for up to 2^53; beyond that accumulate in a BigInt.
    let mut acc: u64 = 0;
    let mut overflow = false;
    for c in digits.chars() {
        let d = c.to_digit(radix).unwrap() as u64;
        match acc.checked_mul(radix as u64).and_then(|x| x.checked_add(d)) {
            Some(v) if v < (1u64 << 53) => acc = v,
            _ => {
                overflow = true;
                break;
            }
        }
    }
    if !overflow {
        return acc as f64;
    }
    let mut big = BigInt::zero();
    for c in digits.chars() {
        big = big * radix + c.to_digit(radix).unwrap();
    }
    crate::bigint::to_f64(&big)
}

impl Vm {
    // ---- primitives -----------------------------------------------------------------

    pub fn to_boolean(&mut self, v: Value) -> bool {
        match v {
            Value::Undefined | Value::Null | Value::Empty => false,
            Value::Bool(b) => b,
            Value::Number(n) => !(n == 0.0 || n.is_nan()),
            Value::String(s) => self.heap.str_len(s) != 0,
            Value::Symbol(_) | Value::Object(_) => true,
            Value::BigInt(b) => !self.heap.bigint(b).is_zero(),
        }
    }

    /// ToPrimitive(input, hint).
    pub fn to_primitive(&mut self, v: Value, hint: Hint) -> JsResult<Value> {
        let Value::Object(o) = v else { return Ok(v) };
        let exotic = self.get_method(v, PropKey::Sym(self.well_known.to_primitive))?;
        if let Some(f) = exotic {
            let h = match hint {
                Hint::Default => self.names.default,
                Hint::Number => self.names.number,
                Hint::String => self.names.string,
            };
            let r = self.call_value(f, v, &[Value::String(h)])?;
            if r.is_object() {
                return Err(self.throw_type("Cannot convert object to primitive value"));
            }
            return Ok(r);
        }
        self.ordinary_to_primitive(
            o,
            if hint == Hint::String {
                Hint::String
            } else {
                Hint::Number
            },
        )
    }

    pub fn ordinary_to_primitive(&mut self, o: ObjId, hint: Hint) -> JsResult<Value> {
        let order = if hint == Hint::String {
            [self.names.toString, self.names.valueOf]
        } else {
            [self.names.valueOf, self.names.toString]
        };
        for name in order {
            let f = self.get_prop(o, PropKey::Str(name), Value::Object(o))?;
            if self.is_callable(f) {
                let r = self.call_value(f, Value::Object(o), &[])?;
                if !r.is_object() {
                    return Ok(r);
                }
            }
        }
        Err(self.throw_type("Cannot convert object to primitive value"))
    }

    pub fn to_number(&mut self, v: Value) -> JsResult<f64> {
        match v {
            Value::Number(n) => Ok(n),
            Value::Undefined | Value::Empty => Ok(f64::NAN),
            Value::Null => Ok(0.0),
            Value::Bool(b) => Ok(if b { 1.0 } else { 0.0 }),
            Value::String(s) => Ok(self.string_to_number(s)),
            Value::Symbol(_) => Err(self.throw_type("Cannot convert a Symbol value to a number")),
            Value::BigInt(_) => Err(self.throw_type("Cannot convert a BigInt value to a number")),
            Value::Object(_) => {
                let p = self.to_primitive(v, Hint::Number)?;
                self.to_number(p)
            }
        }
    }

    pub fn string_to_number(&mut self, s: StrId) -> f64 {
        let u = self.heap.units(s);
        string_to_number(&u)
    }

    /// ToNumeric: a Number or BigInt value.
    pub fn to_numeric(&mut self, v: Value) -> JsResult<Value> {
        let p = self.to_primitive(v, Hint::Number)?;
        if let Value::BigInt(_) = p {
            return Ok(p);
        }
        Ok(Value::Number(self.to_number(p)?))
    }

    pub fn to_integer_or_infinity(&mut self, v: Value) -> JsResult<f64> {
        let n = self.to_number(v)?;
        Ok(to_integer_or_infinity_f64(n))
    }

    pub fn to_int32(&mut self, v: Value) -> JsResult<i32> {
        let n = self.to_number(v)?;
        Ok(f64_to_int32(n))
    }

    pub fn to_uint32(&mut self, v: Value) -> JsResult<u32> {
        let n = self.to_number(v)?;
        Ok(f64_to_uint32(n))
    }

    pub fn to_length(&mut self, v: Value) -> JsResult<f64> {
        let n = self.to_integer_or_infinity(v)?;
        if n <= 0.0 {
            return Ok(0.0);
        }
        Ok(n.min(9007199254740991.0))
    }

    pub fn to_index(&mut self, v: Value) -> JsResult<f64> {
        if v.is_undefined() {
            return Ok(0.0);
        }
        let n = self.to_integer_or_infinity(v)?;
        if !(0.0..=9007199254740991.0).contains(&n) {
            return Err(self.throw_range("Invalid index"));
        }
        Ok(n)
    }

    pub fn to_string(&mut self, v: Value) -> JsResult<StrId> {
        match v {
            Value::String(s) => Ok(s),
            Value::Undefined | Value::Empty => Ok(self.names.undefined),
            Value::Null => Ok(self.names.null),
            Value::Bool(true) => Ok(self.names.true_),
            Value::Bool(false) => Ok(self.names.false_),
            Value::Number(n) => Ok(self.number_to_string(n)),
            Value::Symbol(_) => Err(self.throw_type("Cannot convert a Symbol value to a string")),
            Value::BigInt(b) => {
                let s = self.heap.bigint(b).0.to_string();
                Ok(self.heap.alloc_str(&s))
            }
            Value::Object(_) => {
                let p = self.to_primitive(v, Hint::String)?;
                self.to_string(p)
            }
        }
    }

    pub fn number_to_string(&mut self, n: f64) -> StrId {
        if n.fract() == 0.0 && n.abs() < 1e9 && !(n == 0.0 && n.is_sign_negative()) {
            // Small integers: cheap path.
            let s = (n as i64).to_string();
            return self.heap.alloc_str(&s);
        }
        let s = gable_js_parser::number::to_js_string(n);
        self.heap.alloc_str(&s)
    }

    /// ToString, returning a Rust string.
    pub fn to_rust_string(&mut self, v: Value) -> JsResult<String> {
        let s = self.to_string(v)?;
        Ok(self.heap.str_to_string(s))
    }

    pub fn to_property_key(&mut self, v: Value) -> JsResult<PropKey> {
        match v {
            Value::String(s) => Ok(self.key_from_str(s)),
            Value::Symbol(s) => Ok(PropKey::Sym(s)),
            Value::Number(n) => {
                if n >= 0.0
                    && n < PropKey::MAX_INDEX as f64
                    && n.fract() == 0.0
                    && !(n == 0.0 && n.is_sign_negative())
                {
                    Ok(PropKey::Index(n as u32))
                } else {
                    let s = self.number_to_string(n);
                    Ok(self.key_from_str(s))
                }
            }
            Value::Object(_) => {
                let p = self.to_primitive(v, Hint::String)?;
                self.to_property_key(p)
            }
            other => {
                let s = self.to_string(other)?;
                Ok(self.key_from_str(s))
            }
        }
    }

    pub fn to_object(&mut self, v: Value) -> JsResult<ObjId> {
        let (proto, class) = match v {
            Value::Object(o) => return Ok(o),
            Value::Undefined | Value::Null | Value::Empty => {
                return Err(self.throw_type("Cannot convert undefined or null to object"));
            }
            Value::Bool(b) => (Intrinsic::BooleanPrototype, Class::Boolean(b)),
            Value::Number(n) => (Intrinsic::NumberPrototype, Class::Number(n)),
            Value::String(s) => {
                let proto = self.intrinsic(Intrinsic::StringPrototype);
                return Ok(self.string_object(s, proto));
            }
            Value::Symbol(s) => (Intrinsic::SymbolPrototype, Class::Symbol(s)),
            Value::BigInt(b) => (Intrinsic::BigIntPrototype, Class::BigInt(b)),
        };
        let proto = self.intrinsic(proto);
        Ok(self.alloc(ObjectData::new(Some(proto), class)))
    }

    /// StringCreate: a String wrapper with its `length` property.
    pub fn string_object(&mut self, s: StrId, proto: ObjId) -> ObjId {
        let o = self.alloc(ObjectData::new(Some(proto), Class::String(s)));
        let len = self.heap.str_len(s);
        let k = PropKey::Str(self.names.length);
        self.heap
            .add_own_property(o, k, Property::data(Value::from_usize(len), Attrs::NONE));
        o
    }

    pub fn to_bigint(&mut self, v: Value) -> JsResult<BigId> {
        let p = self.to_primitive(v, Hint::Number)?;
        match p {
            Value::BigInt(b) => Ok(b),
            Value::Bool(b) => Ok(self.alloc_bigint(BigInt::from(b as u8))),
            Value::String(s) => {
                let u = self.heap.units(s);
                match crate::bigint::parse(u.iter()) {
                    Some(b) => Ok(self.alloc_bigint(b)),
                    None => {
                        let text = self.heap.str_to_string(s);
                        Err(self.throw_syntax(&format!("Cannot convert {text} to a BigInt")))
                    }
                }
            }
            Value::Undefined | Value::Null | Value::Empty => {
                Err(self.throw_type("Cannot convert undefined or null to a BigInt"))
            }
            Value::Number(n) => Err(self.throw_type(&format!(
                "Cannot convert {} to a BigInt",
                gable_js_parser::number::to_js_string(n)
            ))),
            Value::Symbol(_) => Err(self.throw_type("Cannot convert a Symbol value to a BigInt")),
            Value::Object(_) => unreachable!(),
        }
    }

    pub fn alloc_bigint(&mut self, b: BigInt) -> BigId {
        self.heap.alloc_bigint(crate::bigint::BigIntData(b))
    }

    pub fn bigint_value(&mut self, b: BigInt) -> Value {
        Value::BigInt(self.alloc_bigint(b))
    }

    // ---- equality ---------------------------------------------------------------------

    pub fn same_value(&mut self, a: Value, b: Value) -> bool {
        match (a, b) {
            (Value::Number(x), Value::Number(y)) => {
                if x.is_nan() && y.is_nan() {
                    return true;
                }
                x == y && x.is_sign_negative() == y.is_sign_negative()
            }
            _ => self.strict_equals(a, b),
        }
    }

    pub fn same_value_zero(&mut self, a: Value, b: Value) -> bool {
        match (a, b) {
            (Value::Number(x), Value::Number(y)) => (x.is_nan() && y.is_nan()) || x == y,
            _ => self.strict_equals(a, b),
        }
    }

    pub fn strict_equals(&mut self, a: Value, b: Value) -> bool {
        match (a, b) {
            (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
            (Value::Empty, Value::Empty) => true,
            (Value::Bool(x), Value::Bool(y)) => x == y,
            (Value::Number(x), Value::Number(y)) => x == y,
            (Value::String(x), Value::String(y)) => x == y || self.heap.str_eq(x, y),
            (Value::Symbol(x), Value::Symbol(y)) => x == y,
            (Value::Object(x), Value::Object(y)) => x == y,
            (Value::BigInt(x), Value::BigInt(y)) => {
                x == y || self.heap.bigint(x).0 == self.heap.bigint(y).0
            }
            _ => false,
        }
    }

    /// IsLooselyEqual.
    pub fn loose_equals(&mut self, a: Value, b: Value) -> JsResult<bool> {
        match (a, b) {
            (Value::Undefined | Value::Null, Value::Undefined | Value::Null) => Ok(true),
            (Value::Undefined | Value::Null, _) | (_, Value::Undefined | Value::Null) => Ok(false),
            (Value::Number(_), Value::String(s)) => {
                let n = self.string_to_number(s);
                Ok(a == Value::Number(n) || self.strict_equals(a, Value::Number(n)))
            }
            (Value::String(s), Value::Number(_)) => {
                let n = self.string_to_number(s);
                Ok(self.strict_equals(Value::Number(n), b))
            }
            (Value::BigInt(x), Value::String(s)) | (Value::String(s), Value::BigInt(x)) => {
                let u = self.heap.units(s);
                match crate::bigint::parse(u.iter()) {
                    Some(n) => Ok(self.heap.bigint(x).0 == n),
                    None => Ok(false),
                }
            }
            (Value::Bool(x), _) => self.loose_equals(Value::Number(x as u8 as f64), b),
            (_, Value::Bool(y)) => self.loose_equals(a, Value::Number(y as u8 as f64)),
            (
                Value::Object(_),
                Value::Number(_) | Value::String(_) | Value::BigInt(_) | Value::Symbol(_),
            ) => {
                let p = self.to_primitive(a, Hint::Default)?;
                self.loose_equals(p, b)
            }
            (
                Value::Number(_) | Value::String(_) | Value::BigInt(_) | Value::Symbol(_),
                Value::Object(_),
            ) => {
                let p = self.to_primitive(b, Hint::Default)?;
                self.loose_equals(a, p)
            }
            (Value::BigInt(x), Value::Number(n)) | (Value::Number(n), Value::BigInt(x)) => {
                if n.is_nan() || n.is_infinite() {
                    return Ok(false);
                }
                Ok(bigint_cmp_f64(&self.heap.bigint(x).0, n) == Some(std::cmp::Ordering::Equal))
            }
            _ => Ok(self.strict_equals(a, b)),
        }
    }

    pub fn typeof_value(&mut self, v: Value) -> StrId {
        match v {
            Value::Undefined | Value::Empty => self.names.undefined,
            Value::Null => self.names.object,
            Value::Bool(_) => self.names.boolean,
            Value::Number(_) => self.names.number,
            Value::String(_) => self.names.string,
            Value::Symbol(_) => self.names.symbol,
            Value::BigInt(_) => self.names.bigint,
            Value::Object(o) => {
                if self.heap.obj(o).is_callable() {
                    self.names.function
                } else {
                    self.names.object
                }
            }
        }
    }

    // ---- operators ----------------------------------------------------------------------

    pub fn op_add(&mut self, l: Value, r: Value) -> JsResult<Value> {
        if let (Value::Number(a), Value::Number(b)) = (l, r) {
            return Ok(Value::Number(a + b));
        }
        if let (Value::String(a), Value::String(b)) = (l, r) {
            return Ok(Value::String(self.heap.concat(a, b)));
        }
        let lp = self.to_primitive(l, Hint::Default)?;
        let rp = self.to_primitive(r, Hint::Default)?;
        if lp.is_string() || rp.is_string() {
            let a = self.to_string(lp)?;
            let b = self.to_string(rp)?;
            return Ok(Value::String(self.heap.concat(a, b)));
        }
        let ln = self.to_numeric(lp)?;
        let rn = self.to_numeric(rp)?;
        match (ln, rn) {
            (Value::Number(a), Value::Number(b)) => Ok(Value::Number(a + b)),
            (Value::BigInt(a), Value::BigInt(b)) => {
                let s = &self.heap.bigint(a).0 + &self.heap.bigint(b).0;
                Ok(self.bigint_value(s))
            }
            _ => {
                Err(self.throw_type("Cannot mix BigInt and other types, use explicit conversions"))
            }
        }
    }

    pub fn op_arith(&mut self, op: ArithOp, l: Value, r: Value) -> JsResult<Value> {
        if let (Value::Number(a), Value::Number(b)) = (l, r) {
            return Ok(Value::Number(number_arith(op, a, b)));
        }
        let ln = self.to_numeric(l)?;
        let rn = self.to_numeric(r)?;
        match (ln, rn) {
            (Value::Number(a), Value::Number(b)) => Ok(Value::Number(number_arith(op, a, b))),
            (Value::BigInt(a), Value::BigInt(b)) => {
                let x = self.heap.bigint(a).0.clone();
                let y = self.heap.bigint(b).0.clone();
                let res = match op {
                    ArithOp::Sub => x - y,
                    ArithOp::Mul => x * y,
                    ArithOp::Div => {
                        if y.is_zero() {
                            return Err(self.throw_range("Division by zero"));
                        }
                        x / y
                    }
                    ArithOp::Mod => {
                        if y.is_zero() {
                            return Err(self.throw_range("Division by zero"));
                        }
                        x % y
                    }
                    ArithOp::Exp => {
                        if y.is_negative() {
                            return Err(self.throw_range("Exponent must be non-negative"));
                        }
                        match crate::bigint::pow(&x, &y) {
                            Some(v) => v,
                            None => return Err(self.throw_range("Maximum BigInt size exceeded")),
                        }
                    }
                    _ => unreachable!(),
                };
                Ok(self.bigint_value(res))
            }
            _ => {
                Err(self.throw_type("Cannot mix BigInt and other types, use explicit conversions"))
            }
        }
    }

    pub fn op_bitwise(&mut self, op: ArithOp, l: Value, r: Value) -> JsResult<Value> {
        let ln = self.to_numeric(l)?;
        let rn = self.to_numeric(r)?;
        match (ln, rn) {
            (Value::Number(a), Value::Number(b)) => {
                let x = f64_to_int32(a);
                let y = f64_to_uint32(b);
                let res = match op {
                    ArithOp::Shl => (x.wrapping_shl(y & 31)) as f64,
                    ArithOp::Shr => (x >> (y & 31)) as f64,
                    ArithOp::UShr => ((x as u32) >> (y & 31)) as f64,
                    ArithOp::BitAnd => (x & y as i32) as f64,
                    ArithOp::BitOr => (x | y as i32) as f64,
                    ArithOp::BitXor => (x ^ y as i32) as f64,
                    _ => unreachable!(),
                };
                Ok(Value::Number(res))
            }
            (Value::BigInt(a), Value::BigInt(b)) => {
                let x = self.heap.bigint(a).0.clone();
                let y = self.heap.bigint(b).0.clone();
                let res = match op {
                    ArithOp::Shl | ArithOp::Shr => {
                        let shift = if op == ArithOp::Shl { y } else { -y };
                        match shift.to_i64() {
                            Some(s) if s >= 0 => {
                                if s > (1 << 30) {
                                    return Err(self.throw_range("Maximum BigInt size exceeded"));
                                }
                                x << (s as usize)
                            }
                            Some(s) => {
                                let s = (-s) as u64;
                                if s > (1 << 40) {
                                    if x.is_negative() {
                                        BigInt::from(-1)
                                    } else {
                                        BigInt::zero()
                                    }
                                } else {
                                    // Arithmetic shift right (floor division).
                                    floor_shr(&x, s as usize)
                                }
                            }
                            None => {
                                if shift.is_negative() {
                                    if x.is_negative() {
                                        BigInt::from(-1)
                                    } else {
                                        BigInt::zero()
                                    }
                                } else {
                                    return Err(self.throw_range("Maximum BigInt size exceeded"));
                                }
                            }
                        }
                    }
                    ArithOp::UShr => {
                        return Err(
                            self.throw_type("BigInts have no unsigned right shift, use >> instead")
                        )
                    }
                    ArithOp::BitAnd => x & y,
                    ArithOp::BitOr => x | y,
                    ArithOp::BitXor => x ^ y,
                    _ => unreachable!(),
                };
                Ok(self.bigint_value(res))
            }
            _ => {
                Err(self.throw_type("Cannot mix BigInt and other types, use explicit conversions"))
            }
        }
    }

    /// IsLessThan(x, y, LeftFirst). `None` is "undefined" (NaN involved).
    pub fn op_less_than(&mut self, x: Value, y: Value, left_first: bool) -> JsResult<Option<bool>> {
        if let (Value::Number(a), Value::Number(b)) = (x, y) {
            if a.is_nan() || b.is_nan() {
                return Ok(None);
            }
            return Ok(Some(a < b));
        }
        let (px, py) = if left_first {
            let px = self.to_primitive(x, Hint::Number)?;
            let py = self.to_primitive(y, Hint::Number)?;
            (px, py)
        } else {
            let py = self.to_primitive(y, Hint::Number)?;
            let px = self.to_primitive(x, Hint::Number)?;
            (px, py)
        };
        if let (Value::String(a), Value::String(b)) = (px, py) {
            self.heap.flatten(a);
            self.heap.flatten(b);
            let ua = self.heap.units_flat(a);
            let ub = self.heap.units_flat(b);
            return Ok(Some(ua.cmp(&ub) == std::cmp::Ordering::Less));
        }
        match (px, py) {
            (Value::BigInt(a), Value::String(s)) => {
                let u = self.heap.units(s);
                let Some(n) = crate::bigint::parse(u.iter()) else {
                    return Ok(None);
                };
                return Ok(Some(self.heap.bigint(a).0 < n));
            }
            (Value::String(s), Value::BigInt(b)) => {
                let u = self.heap.units(s);
                let Some(n) = crate::bigint::parse(u.iter()) else {
                    return Ok(None);
                };
                return Ok(Some(n < self.heap.bigint(b).0));
            }
            _ => {}
        }
        let nx = self.to_numeric(px)?;
        let ny = self.to_numeric(py)?;
        match (nx, ny) {
            (Value::Number(a), Value::Number(b)) => {
                if a.is_nan() || b.is_nan() {
                    Ok(None)
                } else {
                    Ok(Some(a < b))
                }
            }
            (Value::BigInt(a), Value::BigInt(b)) => {
                Ok(Some(self.heap.bigint(a).0 < self.heap.bigint(b).0))
            }
            (Value::BigInt(a), Value::Number(n)) => {
                if n.is_nan() {
                    return Ok(None);
                }
                Ok(Some(
                    bigint_cmp_f64(&self.heap.bigint(a).0, n) == Some(std::cmp::Ordering::Less),
                ))
            }
            (Value::Number(n), Value::BigInt(b)) => {
                if n.is_nan() {
                    return Ok(None);
                }
                Ok(Some(
                    bigint_cmp_f64(&self.heap.bigint(b).0, n) == Some(std::cmp::Ordering::Greater),
                ))
            }
            _ => unreachable!(),
        }
    }

    /// Concatenate a Rust string and a JS string.
    pub fn concat_str(&mut self, prefix: &str, s: StrId) -> StrId {
        let p = self.heap.alloc_str(prefix);
        self.heap.concat(p, s)
    }

    /// Build a JS string from UTF-16 units.
    pub fn string_from_builder(&mut self, b: StrBuilder) -> Value {
        Value::String(self.heap.alloc_str_utf16(b.finish()))
    }

    /// Trim JS whitespace from a string value's units.
    pub fn trim_units(&mut self, s: StrId, start: bool, end: bool) -> Vec<u16> {
        let u = self.heap.units(s);
        let (a, b) = trim_range(&u, start, end);
        u.slice(a, b).to_vec()
    }
}

fn number_arith(op: ArithOp, a: f64, b: f64) -> f64 {
    match op {
        ArithOp::Sub => a - b,
        ArithOp::Mul => a * b,
        ArithOp::Div => a / b,
        ArithOp::Mod => js_fmod(a, b),
        ArithOp::Exp => js_pow(a, b),
        _ => unreachable!(),
    }
}

/// Number::remainder.
pub fn js_fmod(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() || a.is_infinite() || b == 0.0 {
        return f64::NAN;
    }
    if b.is_infinite() || a == 0.0 {
        return a;
    }
    let r = a % b;
    if r == 0.0 && a.is_sign_negative() {
        -0.0
    } else {
        r
    }
}

/// Number::exponentiate.
pub fn js_pow(base: f64, exp: f64) -> f64 {
    if exp.is_nan() {
        return f64::NAN;
    }
    if exp == 0.0 {
        return 1.0;
    }
    if (base == 1.0 || base == -1.0) && exp.is_infinite() {
        return f64::NAN;
    }
    base.powf(exp)
}

/// Compare a BigInt with a finite or infinite number.
pub fn bigint_cmp_f64(b: &BigInt, n: f64) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    if n.is_nan() {
        return None;
    }
    if n == f64::INFINITY {
        return Some(Ordering::Less);
    }
    if n == f64::NEG_INFINITY {
        return Some(Ordering::Greater);
    }
    let fl = n.floor();
    let fi = crate::bigint::from_f64(fl).expect("finite");
    match b.cmp(&fi) {
        Ordering::Equal => {
            if n > fl {
                Some(Ordering::Less)
            } else {
                Some(Ordering::Equal)
            }
        }
        o => Some(o),
    }
}

fn floor_shr(x: &BigInt, s: usize) -> BigInt {
    // BigInt >> is floor division for num-bigint (rounds toward negative infinity).
    x >> s
}

/// Whether a code unit is whitespace or a line terminator (exported for builtins).
pub fn is_trimmable(u: u16) -> bool {
    is_js_whitespace(u)
}
