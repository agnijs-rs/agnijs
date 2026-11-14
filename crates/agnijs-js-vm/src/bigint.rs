//! BigInt storage and the numeric conversions the operators need. The
//! arithmetic itself is `num-bigint`; this module adds the ECMAScript
//! conversions (number <-> BigInt, string parsing with radix prefixes,
//! `asIntN`/`asUintN`).

use num_bigint::{BigInt, Sign};
use num_traits::{One, Signed, ToPrimitive, Zero};

pub struct BigIntData(pub BigInt);

impl BigIntData {
    pub fn is_zero(&self) -> bool {
        self.0.is_zero()
    }

    /// What the memory limit counts for this BigInt: the fixed part plus
    /// its digits. The allocation charge, the collector's live size and
    /// `memory_usage` all use it, so a BigInt that survives a collection
    /// keeps counting at its real size (TASK-103.50).
    #[inline]
    pub fn heap_bytes(&self) -> usize {
        crate::heap::BIGINT_BYTES + self.0.bits().div_ceil(8) as usize
    }
}

/// `NumberToBigInt`: exact integral doubles only.
pub fn from_f64(n: f64) -> Option<BigInt> {
    if !n.is_finite() || n.fract() != 0.0 {
        return None;
    }
    if n.abs() < 9.007_199_254_740_992e15 {
        return Some(BigInt::from(n as i64));
    }
    // Decompose the double exactly.
    let bits = n.to_bits();
    let sign = if bits >> 63 == 1 {
        Sign::Minus
    } else {
        Sign::Plus
    };
    let exp = ((bits >> 52) & 0x7ff) as i64;
    let mantissa = (bits & ((1u64 << 52) - 1)) | (1u64 << 52);
    let shift = exp - 1075;
    let mut v = BigInt::from(mantissa);
    if shift > 0 {
        v <<= shift as usize;
    } else {
        v >>= (-shift) as usize;
    }
    if sign == Sign::Minus {
        v = -v;
    }
    Some(v)
}

/// BigInt to the nearest double (ties to even), as `Number(bigint)`.
pub fn to_f64(b: &BigInt) -> f64 {
    if let Some(i) = b.to_i64() {
        return i as f64;
    }
    // num-bigint's to_f64 rounds correctly for large values and returns
    // infinities when out of range.
    b.to_f64().unwrap_or(if b.is_negative() {
        f64::NEG_INFINITY
    } else {
        f64::INFINITY
    })
}

/// `StringToBigInt`: an optional sign-less integer literal with radix
/// prefix, surrounded by whitespace. Empty string is 0n. `None` is a
/// SyntaxError for the caller.
pub fn parse(units: impl Iterator<Item = u16>) -> Option<BigInt> {
    let s: Vec<u16> = units.collect();
    let (start, end) = crate::string::trim_range(&crate::string::Units::Two(&s), true, true);
    let s = &s[start..end];
    if s.is_empty() {
        return Some(BigInt::zero());
    }
    let (radix, digits): (u32, &[u16]) = if s.len() > 2 && s[0] == b'0' as u16 {
        match s[1] {
            x if x == b'x' as u16 || x == b'X' as u16 => (16, &s[2..]),
            x if x == b'o' as u16 || x == b'O' as u16 => (8, &s[2..]),
            x if x == b'b' as u16 || x == b'B' as u16 => (2, &s[2..]),
            _ => (10, s),
        }
    } else {
        (10, s)
    };
    if digits.is_empty() {
        return None;
    }
    let (neg, digits) = if radix == 10 && (digits[0] == b'-' as u16 || digits[0] == b'+' as u16) {
        (digits[0] == b'-' as u16, &digits[1..])
    } else {
        (false, digits)
    };
    if digits.is_empty() {
        return None;
    }
    let mut v = BigInt::zero();
    for &u in digits {
        let c = char::from_u32(u as u32)?;
        let d = c.to_digit(radix)?;
        v = v * radix + d;
    }
    Some(if neg { -v } else { v })
}

/// Parse a literal from the compiler's constant pool (already validated by
/// the lexer; may carry a radix prefix, no sign).
pub fn parse_literal(text: &str) -> BigInt {
    parse(text.encode_utf16()).expect("invalid BigInt literal in constant pool")
}

/// The most bits a BigInt result may have where the size is the script's
/// choice: the cap `<<` applies to its shift count, used by
/// `BigInt.asUintN` too.
pub const MAX_BITS: u64 = 1 << 30;

/// `x mod 2^bits` for `bits >= 1`, by masking (num-bigint's `&` works on
/// the two's complement of a negative value, as ECMAScript's does).
fn low_bits(bits: u64, x: &BigInt) -> BigInt {
    let mask = (BigInt::one() << bits) - 1;
    x & mask
}

/// `BigInt.asUintN(bits, x)`, or `None` when `x` is negative and `bits` is
/// over [`MAX_BITS`]: that result has `bits` bits, up to 2^53-1 of them.
/// A value that already fits comes back as is, whatever `bits` is, so
/// nothing larger than `x` or `2^bits` is built (TASK-103.44).
pub fn as_uint_n(bits: u64, x: &BigInt) -> Option<BigInt> {
    if bits == 0 {
        return Some(BigInt::zero());
    }
    if !x.is_negative() {
        if x.bits() <= bits {
            return Some(x.clone());
        }
        // `bits` is below the size of `x`: the mask is no larger than it.
        return Some(low_bits(bits, x));
    }
    if bits > MAX_BITS {
        return None;
    }
    Some(low_bits(bits, x))
}

/// `BigInt.asIntN(bits, x)`. The result is never larger than `x`.
pub fn as_int_n(bits: u64, x: &BigInt) -> BigInt {
    if bits == 0 {
        return BigInt::zero();
    }
    // |x| < 2^(bits-1) fits as is: the common case for a huge `bits`.
    if x.bits() < bits {
        return x.clone();
    }
    // Here `bits` is at most the size of `x`.
    let r = low_bits(bits, x);
    if r.bit(bits - 1) {
        r - (BigInt::one() << bits)
    } else {
        r
    }
}

/// `BigInt::toString(radix)`.
pub fn to_string_radix(b: &BigInt, radix: u32) -> String {
    b.to_str_radix(radix)
}

/// At most two fewer code units than `b` takes in `radix` (2 to 36), from
/// its bit length alone: a value of n bits has at least
/// floor((n - 1) / log2(radix)) + 1 digits, and one is dropped for the
/// rounding of the float. The sign counts.
pub fn min_string_len(b: &BigInt, radix: u32) -> usize {
    let bits = b.bits();
    if bits <= 1 {
        return 1;
    }
    let digits = ((bits - 1) as f64 / f64::from(radix).log2()).floor() as usize;
    digits.max(1) + usize::from(b.sign() == num_bigint::Sign::Minus)
}

/// Exponentiation with a non-negative exponent (RangeError otherwise is the
/// caller's job). `None` when the exponent does not fit.
pub fn pow(base: &BigInt, exp: &BigInt) -> Option<BigInt> {
    let e = exp.to_u32()?;
    Some(num_traits::pow::Pow::pow(base, e))
}

/// The low 128 bits of `b` in two's complement (for typed-array stores).
pub fn to_i128_wrapping(b: &BigInt) -> i128 {
    let m = as_int_n(128, b);
    // as_int_n yields a value in [-2^127, 2^127), which fits i128 exactly.
    let (sign, digits) = m.to_u64_digits();
    let mut v: u128 = 0;
    for (i, d) in digits.iter().enumerate().take(2) {
        v |= (*d as u128) << (64 * i);
    }
    match sign {
        num_bigint::Sign::Minus => (v as i128).wrapping_neg(),
        _ => v as i128,
    }
}
