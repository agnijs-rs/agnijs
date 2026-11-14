//! BigInt storage and the numeric conversions the operators need. The
//! arithmetic itself is `num-bigint`; this module adds the ECMAScript
//! conversions (number <-> BigInt, string parsing with radix prefixes,
//! `asIntN`/`asUintN`).

use num_bigint::{BigInt, BigUint, Sign};
use num_integer::Integer;
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
/// prefix, surrounded by whitespace. Empty string is 0n. `Ok(None)` is a
/// SyntaxError for the caller. A power-of-two radix is linear; decimal
/// takes one step per 19 digits, each reported to `poll` (TASK-103.55).
pub fn parse<E>(s: &[u16], poll: Poll<'_, E>) -> Result<Option<BigInt>, E> {
    let (start, end) = crate::string::trim_range(&crate::string::Units::Two(s), true, true);
    let s = &s[start..end];
    if s.is_empty() {
        return Ok(Some(BigInt::zero()));
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
        return Ok(None);
    }
    let (neg, digits) = if radix == 10 && (digits[0] == b'-' as u16 || digits[0] == b'+' as u16) {
        (digits[0] == b'-' as u16, &digits[1..])
    } else {
        (false, digits)
    };
    if digits.is_empty() {
        return Ok(None);
    }
    let mut values = Vec::with_capacity(digits.len());
    for &u in digits {
        match char::from_u32(u32::from(u)).and_then(|c| c.to_digit(radix)) {
            Some(d) => values.push(d as u8),
            None => return Ok(None),
        }
    }
    let v = if radix == 10 {
        let mut v = BigUint::zero();
        for chunk in values.chunks(19) {
            let c = chunk.iter().fold(0u64, |c, &d| c * 10 + u64::from(d));
            v = v * 10u64.pow(chunk.len() as u32) + c;
            poll(digits32(&v))?;
        }
        v
    } else {
        BigUint::from_radix_be(&values, radix).unwrap_or_default()
    };
    let v = BigInt::from(v);
    Ok(Some(if neg { -v } else { v }))
}

/// Parse a literal from the compiler's constant pool (already validated by
/// the lexer; may carry a radix prefix, no sign). The source bounds its
/// length, so it is parsed without polls.
pub fn parse_literal(text: &str) -> BigInt {
    let units: Vec<u16> = text.encode_utf16().collect();
    match parse::<std::convert::Infallible>(&units, &mut |_| Ok(())) {
        Ok(v) => v.expect("invalid BigInt literal in constant pool"),
        Err(e) => match e {},
    }
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

// ---- bounded work between polls (TASK-103.55) -------------------------------------------------
//
// Multiplication, division, decimal parsing and conversion to a non-power-
// of-two radix are superlinear in the digits, and num-bigint does each in
// one call with no poll. Large operands go through the functions below,
// which do the work in steps of bounded cost and report each step's digit
// operations to `poll` (`Vm::poll_work`), which asks the host about the
// interrupt. Small operands take num-bigint's path unchanged.

/// The poll a step reports to: the 32-bit digit operations it did.
pub type Poll<'a, E> = &'a mut dyn FnMut(usize) -> Result<(), E>;

/// About the digit operations of one step: a few milliseconds of work.
const STEP_WORK: usize = 1 << 22;

/// Blocks of this many 32-bit digits multiply in one num-bigint call
/// (Toom-3, about 2^21 digit operations); larger operands multiply block
/// by block.
const MUL_BLOCK: usize = 1 << 15;

/// Below this many bits, conversion to a string is one num-bigint call.
const DIRECT_STRING_BITS: u64 = 1 << 15;

fn digits32(x: &BigUint) -> usize {
    x.bits().div_ceil(32) as usize
}

/// `p` added into `acc` at digit `off`. The caller guarantees the sum fits.
fn add_at(acc: &mut [u32], off: usize, p: &[u32]) {
    let mut carry = 0u64;
    let mut k = off;
    for &d in p {
        let s = u64::from(acc[k]) + u64::from(d) + carry;
        acc[k] = s as u32;
        carry = s >> 32;
        k += 1;
    }
    while carry != 0 {
        let s = u64::from(acc[k]) + carry;
        acc[k] = s as u32;
        carry = s >> 32;
        k += 1;
    }
}

/// `x * y`, block by block when both operands are large.
pub fn mul_polled<E>(x: &BigInt, y: &BigInt, poll: Poll<'_, E>) -> Result<BigInt, E> {
    let (xl, yl) = (digits32(x.magnitude()), digits32(y.magnitude()));
    if xl.min(yl) <= 32 || xl.max(yl) <= 2 * MUL_BLOCK {
        let p = x * y;
        // Squaring up to a large power is a run of these.
        poll(xl.saturating_mul(yl) >> 6)?;
        return Ok(p);
    }
    let a = x.magnitude().to_u32_digits();
    let b = y.magnitude().to_u32_digits();
    let mut acc = vec![0u32; a.len() + b.len()];
    for (i, ac) in a.chunks(MUL_BLOCK).enumerate() {
        let ac = BigUint::from_slice(ac);
        for (j, bc) in b.chunks(MUL_BLOCK).enumerate() {
            let p = &ac * BigUint::from_slice(bc);
            add_at(&mut acc, (i + j) * MUL_BLOCK, &p.to_u32_digits());
            poll(STEP_WORK)?;
        }
    }
    Ok(BigInt::from_biguint(x.sign() * y.sign(), BigUint::new(acc)))
}

/// Truncating `(x / y, x % y)` for a non-zero `y`: long division by
/// blocks of the dividend's digits when the work is large, each block one
/// num-bigint division of bounded cost.
pub fn div_rem_polled<E>(x: &BigInt, y: &BigInt, poll: Poll<'_, E>) -> Result<(BigInt, BigInt), E> {
    let d = y.magnitude();
    let (n, m) = (digits32(x.magnitude()), digits32(d));
    if n < m || (n - m + 1).saturating_mul(m) <= STEP_WORK {
        return Ok(x.div_rem(y));
    }
    let k = (STEP_WORK / m).max(1);
    let digits = x.magnitude().to_u32_digits();
    let mut q = vec![0u32; digits.len()];
    let mut rem = BigUint::zero();
    let mut hi = digits.len();
    while hi > 0 {
        let lo = hi.saturating_sub(k);
        // rem < d, so this block's quotient has at most hi - lo digits.
        let t = (rem << (32 * (hi - lo))) + BigUint::from_slice(&digits[lo..hi]);
        let (qc, r) = t.div_rem(d);
        for (i, w) in qc.to_u32_digits().into_iter().enumerate() {
            q[lo + i] = w;
        }
        rem = r;
        poll((hi - lo + 1) * m)?;
        hi = lo;
    }
    Ok((
        BigInt::from_biguint(x.sign() * y.sign(), BigUint::new(q)),
        BigInt::from_biguint(x.sign(), rem),
    ))
}

/// The digits of `b` in `radix` (2 to 36), as `BigInt::toString`. A power-
/// of-two radix is linear; any other takes one step per 64-bit chunk of
/// the result.
pub fn to_string_polled<E>(b: &BigInt, radix: u32, poll: Poll<'_, E>) -> Result<String, E> {
    if radix.is_power_of_two() || b.bits() <= DIRECT_STRING_BITS {
        return Ok(b.to_str_radix(radix));
    }
    // The largest power of the radix that fits a u64, and its exponent.
    let r = u64::from(radix);
    let (mut base, mut k) = (r, 1usize);
    while let Some(next) = base.checked_mul(r) {
        base = next;
        k += 1;
    }
    let base = BigUint::from(base);
    let mut v = b.magnitude().clone();
    let mut chunks: Vec<u64> = Vec::new();
    while !v.is_zero() {
        poll(digits32(&v))?;
        let (q, c) = v.div_rem(&base);
        chunks.push(c.to_u64().unwrap_or(0));
        v = q;
    }
    let mut s = String::with_capacity(chunks.len() * k + 1);
    if b.is_negative() {
        s.push('-');
    }
    let mut buf = ['0'; 64];
    for (i, &c) in chunks.iter().rev().enumerate() {
        let (mut c, mut len) = (c, 0);
        loop {
            buf[len] = char::from_digit((c % r) as u32, radix).unwrap_or('0');
            c /= r;
            len += 1;
            if c == 0 {
                break;
            }
        }
        if i > 0 {
            for _ in len..k {
                s.push('0');
            }
        }
        s.extend(buf[..len].iter().rev());
    }
    Ok(s)
}

/// An upper bound on the bits of `base ** exp` (`exp` not negative), or
/// `None` past [`MAX_BITS`], the cap `<<` applies: at least
/// `(bits(base) - 1) * exp + 1` bits. Bases 0, 1 and -1 never grow.
pub fn pow_bits(base: &BigInt, exp: &BigInt) -> Option<u64> {
    if exp.is_zero() || base.magnitude().is_one() || base.is_zero() {
        return Some(1);
    }
    let e = exp.to_u64()?;
    let bits = base.bits();
    if (bits - 1).checked_mul(e)? > MAX_BITS {
        return None;
    }
    bits.checked_mul(e)
}

/// `base ** exp` for an `exp` that [`pow_bits`] accepted, by squaring,
/// each product through [`mul_polled`].
pub fn pow_polled<E>(base: &BigInt, exp: &BigInt, poll: Poll<'_, E>) -> Result<BigInt, E> {
    if exp.is_zero() {
        return Ok(BigInt::one());
    }
    if base.is_zero() {
        return Ok(BigInt::zero());
    }
    if base.magnitude().is_one() {
        return Ok(if base.is_negative() && exp.bit(0) { -BigInt::one() } else { BigInt::one() });
    }
    let e = exp.to_u64().unwrap_or(0);
    let mut acc = base.clone();
    for i in (0..63 - e.leading_zeros()).rev() {
        acc = mul_polled(&acc, &acc, poll)?;
        if (e >> i) & 1 == 1 {
            acc = mul_polled(&acc, base, poll)?;
        }
    }
    Ok(acc)
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
