//! Exact decimal values and the ECMA-402 rounding operations (ToRawFixed,
//! ToRawPrecision) on them. Numbers reach the Intl built-ins as
//! mathematical values; this type holds them exactly so rounding follows
//! the specification rather than binary floating point.

use num_bigint::BigUint;
use num_integer::Integer;
use num_traits::{One, ToPrimitive, Zero};

/// `mant × 10^exp`, with `mant` free of leading zeros (empty for zero).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dec {
    pub neg: bool,
    pub mant: Vec<u8>,
    pub exp: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoundingMode {
    Ceil,
    Floor,
    Expand,
    Trunc,
    HalfCeil,
    HalfFloor,
    HalfExpand,
    HalfTrunc,
    HalfEven,
}

impl RoundingMode {
    pub fn parse(s: &str) -> Option<RoundingMode> {
        Some(match s {
            "ceil" => RoundingMode::Ceil,
            "floor" => RoundingMode::Floor,
            "expand" => RoundingMode::Expand,
            "trunc" => RoundingMode::Trunc,
            "halfCeil" => RoundingMode::HalfCeil,
            "halfFloor" => RoundingMode::HalfFloor,
            "halfExpand" => RoundingMode::HalfExpand,
            "halfTrunc" => RoundingMode::HalfTrunc,
            "halfEven" => RoundingMode::HalfEven,
            _ => return None,
        })
    }
}

impl Dec {
    pub fn zero() -> Dec {
        Dec { neg: false, mant: Vec::new(), exp: 0 }
    }

    pub fn is_zero(&self) -> bool {
        self.mant.is_empty()
    }

    /// The exact value of a finite double.
    pub fn from_f64(x: f64) -> Dec {
        let neg = x.is_sign_negative();
        let x = x.abs();
        if x == 0.0 {
            return Dec { neg, mant: Vec::new(), exp: 0 };
        }
        let bits = x.to_bits();
        let exp_bits = ((bits >> 52) & 0x7ff) as i32;
        let frac = bits & ((1u64 << 52) - 1);
        let (m, e) = if exp_bits == 0 {
            (frac, -1074)
        } else {
            (frac | (1u64 << 52), exp_bits - 1075)
        };
        let mut d = if e >= 0 {
            let big = BigUint::from(m) << (e as usize);
            Dec::from_biguint(big, 0)
        } else {
            let k = (-e) as usize;
            let big = BigUint::from(m) * BigUint::from(5u32).pow(k as u32);
            Dec::from_biguint(big, -(k as i32))
        };
        d.neg = neg;
        d.normalize();
        d.strip_trailing_zeros();
        d
    }

    fn from_biguint(big: BigUint, exp: i32) -> Dec {
        let s = big.to_string();
        let mant: Vec<u8> = if s == "0" { Vec::new() } else { s.bytes().map(|b| b - b'0').collect() };
        Dec { neg: false, mant, exp }
    }

    /// Parse a decimal literal: optional sign, digits with an optional
    /// fraction, optional exponent. `None` when the text is not one.
    pub fn parse(s: &str) -> Option<Dec> {
        let s = s.trim();
        let (neg, rest) = match s.as_bytes().first()? {
            b'-' => (true, &s[1..]),
            b'+' => (false, &s[1..]),
            _ => (false, s),
        };
        let (num, exp_part) = match rest.find(['e', 'E']) {
            Some(i) => (&rest[..i], Some(&rest[i + 1..])),
            None => (rest, None),
        };
        let (int_part, frac_part) = match num.find('.') {
            Some(i) => (&num[..i], &num[i + 1..]),
            None => (num, ""),
        };
        if int_part.is_empty() && frac_part.is_empty() {
            return None;
        }
        if !int_part.bytes().all(|b| b.is_ascii_digit()) || !frac_part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let mut exp: i32 = match exp_part {
            Some(e) => {
                let e = e.trim();
                if e.is_empty() {
                    return None;
                }
                e.parse::<i32>().ok()?
            }
            None => 0,
        };
        exp -= frac_part.len() as i32;
        let mant: Vec<u8> = int_part.bytes().chain(frac_part.bytes()).map(|b| b - b'0').collect();
        let mut d = Dec { neg, mant, exp };
        d.normalize();
        Some(d)
    }

    fn normalize(&mut self) {
        let lead = self.mant.iter().take_while(|&&d| d == 0).count();
        self.mant.drain(..lead);
        if self.mant.is_empty() {
            self.exp = 0;
        }
    }

    /// Drop trailing zero digits of the mantissa (keeps the value).
    pub fn strip_trailing_zeros(&mut self) {
        while self.mant.last() == Some(&0) {
            self.mant.pop();
            self.exp += 1;
        }
        if self.mant.is_empty() {
            self.exp = 0;
        }
    }

    /// floor(log10(|x|)); 0 for zero.
    pub fn magnitude(&self) -> i32 {
        if self.mant.is_empty() {
            0
        } else {
            self.mant.len() as i32 - 1 + self.exp
        }
    }

    /// Multiply by 10^k.
    pub fn shift(&mut self, k: i32) {
        if !self.mant.is_empty() {
            self.exp += k;
        }
    }

    /// Does the value have no fraction digits (an integer)?
    pub fn is_integer(&self) -> bool {
        if self.exp >= 0 {
            return true;
        }
        let frac = (-self.exp) as usize;
        if frac > self.mant.len() {
            return self.mant.iter().all(|&d| d == 0);
        }
        self.mant[self.mant.len() - frac..].iter().all(|&d| d == 0)
    }

    fn big(&self) -> BigUint {
        let mut b = BigUint::zero();
        for &d in &self.mant {
            b = b * 10u32 + d;
        }
        b
    }

    /// Round so that the last kept digit sits at power-of-ten `pos`, in
    /// steps of `increment × 10^pos`. The mantissa afterwards ends exactly
    /// at `pos` (trailing zeros are kept so callers can count digits).
    pub fn round_to_position(&mut self, pos: i32, mode: RoundingMode, increment: u32) {
        if self.mant.is_empty() {
            self.exp = pos;
            return;
        }
        // x = mant × 10^exp; quantum q = increment × 10^pos.
        // n = x / q as a rational num/den.
        let mut num = self.big();
        let mut den = BigUint::from(increment.max(1));
        let shift = self.exp - pos;
        if shift >= 0 {
            num *= BigUint::from(10u32).pow(shift as u32);
        } else {
            den *= BigUint::from(10u32).pow((-shift) as u32);
        }
        let (mut n, r) = num.div_rem(&den);
        if !r.is_zero() {
            let twice = &r * 2u32;
            let cmp = twice.cmp(&den);
            let up = match mode {
                RoundingMode::Ceil => !self.neg,
                RoundingMode::Floor => self.neg,
                RoundingMode::Expand => true,
                RoundingMode::Trunc => false,
                RoundingMode::HalfCeil => match cmp {
                    std::cmp::Ordering::Greater => true,
                    std::cmp::Ordering::Less => false,
                    std::cmp::Ordering::Equal => !self.neg,
                },
                RoundingMode::HalfFloor => match cmp {
                    std::cmp::Ordering::Greater => true,
                    std::cmp::Ordering::Less => false,
                    std::cmp::Ordering::Equal => self.neg,
                },
                RoundingMode::HalfExpand => cmp != std::cmp::Ordering::Less,
                RoundingMode::HalfTrunc => cmp == std::cmp::Ordering::Greater,
                RoundingMode::HalfEven => match cmp {
                    std::cmp::Ordering::Greater => true,
                    std::cmp::Ordering::Less => false,
                    std::cmp::Ordering::Equal => (&n % 2u32) == BigUint::one(),
                },
            };
            if up {
                n += 1u32;
            }
        }
        let scaled = n * BigUint::from(increment.max(1));
        let s = scaled.to_string();
        self.mant = if s == "0" { Vec::new() } else { s.bytes().map(|b| b - b'0').collect() };
        self.exp = pos;
        if self.mant.is_empty() {
            self.exp = pos;
        }
    }

    /// Render as plain digits with at least `min_fraction` fraction digits
    /// and at least `min_integer` integer digits; mantissa trailing zeros
    /// are kept.
    pub fn to_plain(&self, min_integer: usize, min_fraction: usize) -> String {
        let mut digits: Vec<u8> = self.mant.clone();
        let mut exp = self.exp;
        if digits.is_empty() {
            digits.push(0);
            exp = exp.min(0);
        }
        // Make exp <= 0 so the fraction is explicit.
        if exp > 0 {
            digits.extend(std::iter::repeat(0).take(exp as usize));
            exp = 0;
        }
        let frac_len = (-exp) as usize;
        let (int_digits, frac_digits): (Vec<u8>, Vec<u8>) = if frac_len >= digits.len() {
            let mut f = vec![0u8; frac_len - digits.len()];
            f.extend(&digits);
            (vec![0], f)
        } else {
            let split = digits.len() - frac_len;
            (digits[..split].to_vec(), digits[split..].to_vec())
        };
        let mut out = String::new();
        let int_str: String = int_digits.iter().map(|d| (b'0' + d) as char).collect();
        let int_str = int_str.trim_start_matches('0');
        let int_str = if int_str.is_empty() { "0" } else { int_str };
        for _ in int_str.len()..min_integer {
            out.push('0');
        }
        out.push_str(int_str);
        let mut frac: String = frac_digits.iter().map(|d| (b'0' + d) as char).collect();
        while frac.len() < min_fraction {
            frac.push('0');
        }
        if !frac.is_empty() {
            out.push('.');
            out.push_str(&frac);
        }
        out
    }

    /// The number of fraction digits the mantissa carries.
    pub fn fraction_digits(&self) -> usize {
        if self.exp >= 0 {
            0
        } else {
            (-self.exp) as usize
        }
    }

    /// Drop fraction zeros while more than `keep` fraction digits remain.
    pub fn trim_fraction_zeros_to(&mut self, keep: usize) {
        while self.fraction_digits() > keep && self.mant.last() == Some(&0) {
            self.mant.pop();
            self.exp += 1;
        }
        if self.mant.is_empty() {
            self.exp = -(keep as i32);
        }
    }

    /// An approximation as f64 (for plural operands of huge values).
    pub fn to_f64(&self) -> f64 {
        let s = self.to_plain(1, 0);
        let v = s.parse::<f64>().unwrap_or(0.0);
        if self.neg {
            -v
        } else {
            v
        }
    }

    pub fn to_biguint_integer_part(&self) -> BigUint {
        let s = self.to_plain(1, 0);
        let int = s.split('.').next().unwrap_or("0");
        int.parse::<BigUint>().unwrap_or_else(|_| BigUint::zero())
    }
}

impl Dec {
    /// A rounding target: how many digits and of what kind.
    pub fn significant_digits(&self) -> usize {
        self.mant.len()
    }
}

/// ToRawPrecision: round to `max_p` significant digits, keep at least
/// `min_p`. Returns the rounded value with trailing zeros trimmed to the
/// minimum, and the rounding magnitude (power of ten of the last digit).
pub fn to_raw_precision(x: &Dec, min_p: usize, max_p: usize, mode: RoundingMode) -> (Dec, i32) {
    let mut d = x.clone();
    if d.is_zero() {
        // m is p zeros; e = 0.
        d.mant = vec![0; max_p];
        d.exp = -(max_p as i32 - 1);
        d.trim_fraction_zeros_to(min_p.saturating_sub(1));
        return (d, 0);
    }
    let e = d.magnitude();
    let pos = e - max_p as i32 + 1;
    d.round_to_position(pos, mode, 1);
    // Rounding may have carried into a new digit (9.99 -> 10.0).
    if d.mant.len() > max_p {
        d.mant.pop();
        d.exp += 1;
    }
    let e = d.magnitude();
    // Keep min_p significant digits; drop fraction zeros beyond them.
    let keep_frac = if (min_p as i32) - 1 - e > 0 { (min_p as i32 - 1 - e) as usize } else { 0 };
    d.trim_fraction_zeros_to(keep_frac);
    (d, e - max_p as i32 + 1)
}

/// ToRawFixed: round to `max_f` fraction digits in steps of `increment`
/// units of the last place; keep at least `min_f` fraction digits.
pub fn to_raw_fixed(x: &Dec, min_f: usize, max_f: usize, increment: u32, mode: RoundingMode) -> (Dec, i32) {
    let mut d = x.clone();
    d.round_to_position(-(max_f as i32), mode, increment);
    if d.is_zero() {
        d.exp = -(max_f as i32);
    }
    d.trim_fraction_zeros_to(min_f);
    (d, -(max_f as i32))
}

#[allow(dead_code)]
fn _assert_to_primitive(b: &BigUint) -> Option<u64> {
    b.to_u64()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Dec {
        Dec::parse(s).unwrap()
    }

    #[test]
    fn exact_doubles() {
        assert_eq!(Dec::from_f64(0.5).to_plain(1, 0), "0.5");
        assert_eq!(Dec::from_f64(1e21).to_plain(1, 0), "1000000000000000000000");
        assert!(Dec::from_f64(0.1).to_plain(1, 0).starts_with("0.1000000000000000055511"));
        assert_eq!(Dec::from_f64(-0.0), Dec { neg: true, mant: vec![], exp: 0 });
    }

    #[test]
    fn fixed_rounding() {
        let (r, _) = to_raw_fixed(&d("1.005"), 0, 2, 1, RoundingMode::HalfExpand);
        assert_eq!(r.to_plain(1, 2), "1.01");
        let (r, _) = to_raw_fixed(&d("1.5"), 0, 0, 1, RoundingMode::HalfEven);
        assert_eq!(r.to_plain(1, 0), "2");
        let (r, _) = to_raw_fixed(&d("2.5"), 0, 0, 1, RoundingMode::HalfEven);
        assert_eq!(r.to_plain(1, 0), "2");
        let (r, _) = to_raw_fixed(&d("1.1"), 0, 0, 1, RoundingMode::Ceil);
        assert_eq!(r.to_plain(1, 0), "2");
        let (r, _) = to_raw_fixed(&d("-1.1"), 0, 0, 1, RoundingMode::Ceil);
        assert_eq!(r.to_plain(1, 0), "1");
        let (r, _) = to_raw_fixed(&d("1.13"), 0, 2, 5, RoundingMode::HalfExpand);
        assert_eq!(r.to_plain(1, 2), "1.15");
        let (r, _) = to_raw_fixed(&d("1.2"), 3, 3, 1, RoundingMode::HalfExpand);
        assert_eq!(r.to_plain(1, 3), "1.200");
        let (r, _) = to_raw_fixed(&d("0.0001"), 0, 2, 1, RoundingMode::HalfExpand);
        assert!(r.is_zero());
    }

    #[test]
    fn precision_rounding() {
        let (r, _) = to_raw_precision(&d("123.456"), 1, 3, RoundingMode::HalfExpand);
        assert_eq!(r.to_plain(1, 0), "123");
        let (r, _) = to_raw_precision(&d("9.995"), 1, 3, RoundingMode::HalfExpand);
        assert_eq!(r.to_plain(1, 0), "10");
        let (r, _) = to_raw_precision(&d("9.995"), 3, 3, RoundingMode::HalfExpand);
        assert_eq!(r.to_plain(1, 0), "10.0");
        let (r, _) = to_raw_precision(&d("0.00012345"), 1, 2, RoundingMode::HalfExpand);
        assert_eq!(r.to_plain(1, 0), "0.00012");
        let (r, _) = to_raw_precision(&d("0"), 2, 3, RoundingMode::HalfExpand);
        assert_eq!(r.to_plain(1, 0), "0.0");
        let (r, _) = to_raw_precision(&d("123456"), 1, 3, RoundingMode::HalfExpand);
        assert_eq!(r.to_plain(1, 0), "123000");
    }
}
