//! `Number::toString` for literal property keys (`{ 1.5: x }` has the key
//! "1.5", `{ 1e21: x }` has "1e+21"), shared with the VM.

/// Room for the longest radix-10 form, `-1.2345678901234567e-308` and
/// `-0.0000012345678901234567` both fit.
pub const NUMBER_BUF_LEN: usize = 32;

/// ECMAScript `Number::toString(x)` with radix 10.
pub fn to_js_string(x: f64) -> String {
    let mut buf = [0u8; NUMBER_BUF_LEN];
    format_js_number(x, &mut buf).to_string()
}

/// A fixed buffer for `{:e}` output (no heap allocation).
struct StackWriter<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl core::fmt::Write for StackWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let end = self.len + s.len();
        if end > self.buf.len() {
            return Err(core::fmt::Error);
        }
        self.buf[self.len..end].copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// ECMAScript `Number::toString(x)` with radix 10, written into `buf`
/// without allocating. The result is ASCII.
pub fn format_js_number(x: f64, buf: &mut [u8; NUMBER_BUF_LEN]) -> &str {
    let len = format_into(x, buf);
    core::str::from_utf8(&buf[..len]).expect("number text is ASCII")
}

fn put(buf: &mut [u8], len: &mut usize, s: &[u8]) {
    buf[*len..*len + s.len()].copy_from_slice(s);
    *len += s.len();
}

/// Decimal digits of `v`, written at `buf[len..]`.
fn put_u64(buf: &mut [u8], len: &mut usize, mut v: u64) {
    let mut tmp = [0u8; 20];
    let mut i = tmp.len();
    loop {
        i -= 1;
        tmp[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    put(buf, len, &tmp[i..]);
}

/// The most digits a shortest round-tripping form of an f64 has.
pub const SHORTEST_DIGITS_LEN: usize = 17;

/// The shortest decimal digits that round-trip to `x` (finite, > 0), as
/// ASCII in `digits[..k]`, with `x = d1.d2d3... * 10^exp`: returns
/// `(k, exp)`. When two k-digit numbers round-trip and are equally close to
/// `x`, the even one is taken (the specification's recommendation in
/// Number::toString, and V8's choice; TASK-103.59).
pub fn shortest_digits(x: f64, digits: &mut [u8; SHORTEST_DIGITS_LEN]) -> (usize, i32) {
    // Rust's `{:e}` gives the shortest digits, rounding a tie up.
    let mut sci = [0u8; NUMBER_BUF_LEN];
    let mut w = StackWriter { buf: &mut sci, len: 0 };
    core::fmt::write(&mut w, format_args!("{x:e}")).expect("an f64 in {:e} fits the buffer");
    let sci_len = w.len;
    let sci = &sci[..sci_len];
    let e_at = sci.iter().position(|&c| c == b'e').expect("{:e} has an exponent");
    let mut k = 0usize;
    for &c in &sci[..e_at] {
        if c != b'.' {
            digits[k] = c;
            k += 1;
        }
    }
    let exp_text = core::str::from_utf8(&sci[e_at + 1..]).expect("ASCII");
    let exp: i32 = exp_text.parse().expect("{:e} exponent is an integer");
    // Only odd digits can need the even neighbour, and only an exact
    // decimal of at most 18 digits can be a tie (see `exact_decimal`).
    if digits[k - 1] % 2 == 1 {
        if let Some((n, s)) = exact_decimal(x) {
            break_tie_to_even(x, &mut digits[..k], exp, n, s);
        }
    }
    (k, exp)
}

/// 5^0 to 5^25; 5^26 is over 10^18.
const POW5: [u64; 26] = {
    let mut t = [1u64; 26];
    let mut i = 1;
    while i < 26 {
        t[i] = t[i - 1] * 5;
        i += 1;
    }
    t
};

/// `x`'s (finite, > 0) exact value as `n * 10^q` when it has at most 18
/// digits, the most a tie between two shortest candidates (17 digits at
/// most) can have. With `x = m * 2^q` and `m` odd, the exact form is
/// `(m * 5^-q) * 10^q` for `q < 0`. For `q >= 0` there is no tie: the two
/// candidates would be `5 * 10^q` from `x`, more than the half-ulp
/// (at most `2^(q-1)`) that still rounds back to `x`.
#[inline]
fn exact_decimal(x: f64) -> Option<(u64, i32)> {
    let bits = x.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1 << 52) - 1);
    let (m, q) = if biased == 0 { (frac, -1074) } else { (frac | (1 << 52), biased - 1075) };
    let tz = m.trailing_zeros();
    let (m, q) = (m >> tz, q + tz as i32);
    if !(-25..0).contains(&q) {
        return None;
    }
    let n = m.checked_mul(POW5[q.unsigned_abs() as usize])?;
    (n < 1_000_000_000_000_000_000).then_some((n, q))
}

/// Digits `digits` (shortest for `x`, `x = d1.d2... * 10^exp`) are odd and
/// `x = n * 10^s` exactly: if `x` lies halfway between the digits and the
/// even neighbour that also has `digits.len()` digits, take the neighbour
/// when it round-trips.
#[cold]
#[inline(never)]
fn break_tie_to_even(x: f64, digits: &mut [u8], exp: i32, n: u64, s: i32) {
    let k = digits.len();
    // A tie: n has one digit more than the shortest form and ends in 5.
    if n % 10 != 5 || decimal_len(n) != k + 1 || decimal_len(n) as i32 - 1 + s != exp {
        return;
    }
    let value = digits.iter().fold(0u64, |v, &c| v * 10 + u64::from(c - b'0'));
    let lower = n / 10;
    let other = if value == lower {
        lower + 1
    } else if value == lower + 1 {
        lower
    } else {
        return;
    };
    // The neighbour ends in 0 only past a carry; a shorter form would then
    // round-trip and the shortest digits would not have k of them.
    if other % 10 == 0 || decimal_len(other) != k {
        return;
    }
    // Both are as far from x as each other, and the digits round-trip, so
    // the neighbour does too unless x's rounding interval is lopsided: a
    // power-of-two significand has the double below it twice as close.
    // Then parse the neighbour back.
    if x.to_bits() & ((1 << 52) - 1) == 0 && x.to_bits() >> 52 > 1 {
        let mut text = [0u8; NUMBER_BUF_LEN];
        let mut w = StackWriter { buf: &mut text, len: 0 };
        core::fmt::write(&mut w, format_args!("{other}e{}", exp - (k as i32 - 1)))
            .expect("the neighbour fits the buffer");
        let len = w.len;
        let parsed: f64 = core::str::from_utf8(&text[..len]).expect("ASCII").parse().expect("a decimal");
        if parsed != x {
            return;
        }
    }
    let mut v = other;
    for d in digits.iter_mut().rev() {
        *d = b'0' + (v % 10) as u8;
        v /= 10;
    }
}

fn decimal_len(n: u64) -> usize {
    n.checked_ilog10().map_or(1, |l| l as usize + 1)
}

fn format_into(x: f64, buf: &mut [u8; NUMBER_BUF_LEN]) -> usize {
    let mut len = 0;
    if x.is_nan() {
        put(buf, &mut len, b"NaN");
        return len;
    }
    if x == 0.0 {
        put(buf, &mut len, b"0");
        return len;
    }
    if x.is_infinite() {
        put(buf, &mut len, if x > 0.0 { b"Infinity" } else { b"-Infinity" });
        return len;
    }
    let mut x = x;
    if x < 0.0 {
        put(buf, &mut len, b"-");
        x = -x;
    }
    // Integers below 2^53 print as their exact digits (k <= n <= 21).
    if x.fract() == 0.0 && x < 9007199254740992.0 {
        put_u64(buf, &mut len, x as u64);
        return len;
    }
    let mut digits = [0u8; SHORTEST_DIGITS_LEN];
    let (k, exp) = shortest_digits(x, &mut digits);
    let digits = &digits[..k];
    let k = k as i32;
    let n = exp + 1; // position of the decimal point relative to digits
    if k <= n && n <= 21 {
        put(buf, &mut len, digits);
        for _ in 0..(n - k) {
            put(buf, &mut len, b"0");
        }
    } else if 0 < n && n <= 21 {
        let (a, b) = digits.split_at(n as usize);
        put(buf, &mut len, a);
        put(buf, &mut len, b".");
        put(buf, &mut len, b);
    } else if -6 < n && n <= 0 {
        put(buf, &mut len, b"0.");
        for _ in 0..(-n) {
            put(buf, &mut len, b"0");
        }
        put(buf, &mut len, digits);
    } else {
        let e = n - 1;
        put(buf, &mut len, &digits[..1]);
        if k > 1 {
            put(buf, &mut len, b".");
            put(buf, &mut len, &digits[1..]);
        }
        put(buf, &mut len, if e < 0 { b"e-" } else { b"e+" });
        put_u64(buf, &mut len, e.unsigned_abs() as u64);
    }
    len
}

#[cfg(test)]
mod tests {
    use super::to_js_string;

    /// The allocating implementation `format_js_number` replaced
    /// (TASK-100.29), kept as the oracle.
    fn reference(x: f64) -> String {
        if x.is_nan() {
            return "NaN".into();
        }
        if x == 0.0 {
            return "0".into();
        }
        if x.is_infinite() {
            return if x > 0.0 { "Infinity".into() } else { "-Infinity".into() };
        }
        if x < 0.0 {
            return format!("-{}", reference(-x));
        }
        let sci = format!("{x:e}");
        let (mantissa, exp) = sci.split_once('e').unwrap();
        let exp: i32 = exp.parse().unwrap();
        let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
        let digits = even_on_a_tie(x, digits, exp);
        let k = digits.len() as i32;
        let n = exp + 1;
        if k <= n && n <= 21 {
            let mut s = digits;
            for _ in 0..(n - k) {
                s.push('0');
            }
            s
        } else if 0 < n && n <= 21 {
            let (a, b) = digits.split_at(n as usize);
            format!("{a}.{b}")
        } else if -6 < n && n <= 0 {
            let mut s = String::from("0.");
            for _ in 0..(-n) {
                s.push('0');
            }
            s.push_str(&digits);
            s
        } else {
            let e = n - 1;
            let sign = if e < 0 { '-' } else { '+' };
            if k == 1 {
                format!("{digits}e{sign}{}", e.abs())
            } else {
                let (a, b) = digits.split_at(1);
                format!("{a}.{b}e{sign}{}", e.abs())
            }
        }
    }

    /// The tie rule worked out the slow way: `x`'s exact decimal expansion
    /// (`{:.800e}` is exact for every f64) is the midpoint of two k-digit
    /// numbers when it has k + 1 digits ending in 5; then the even one is
    /// taken if it round-trips.
    fn even_on_a_tie(x: f64, digits: String, exp: i32) -> String {
        let exact = format!("{x:.800e}");
        let (mantissa, exact_exp) = exact.split_once('e').unwrap();
        let exact_digits: String = mantissa.chars().filter(|c| *c != '.').collect();
        let exact_digits = exact_digits.trim_end_matches('0');
        if exact_exp.parse::<i32>().unwrap() != exp
            || exact_digits.len() != digits.len() + 1
            || !exact_digits.ends_with('5')
        {
            return digits;
        }
        let lower: u64 = exact_digits[..digits.len()].parse().unwrap();
        for candidate in [lower, lower + 1] {
            let text = candidate.to_string();
            if candidate % 2 == 0 && text.len() == digits.len() {
                let value: f64 = format!("{}.{}e{exp}", &text[..1], &text[1..]).parse().unwrap();
                if value == x {
                    return text;
                }
            }
        }
        digits
    }

    #[test]
    fn matches_the_spec_examples() {
        assert_eq!(to_js_string(1.0), "1");
        assert_eq!(to_js_string(1.5), "1.5");
        assert_eq!(to_js_string(100.0), "100");
        assert_eq!(to_js_string(1e21), "1e+21");
        assert_eq!(to_js_string(1e20), "100000000000000000000");
        assert_eq!(to_js_string(0.000001), "0.000001");
        assert_eq!(to_js_string(1e-7), "1e-7");
        assert_eq!(to_js_string(1.5e-7), "1.5e-7");
        assert_eq!(to_js_string(-0.0), "0");
        assert_eq!(to_js_string(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(
            to_js_string(123456789012345680000.0),
            "123456789012345680000"
        );
        assert_eq!(to_js_string(9007199254740991.0), "9007199254740991");
        assert_eq!(to_js_string(9007199254740992.0), "9007199254740992");
        assert_eq!(to_js_string(2f64.powi(60)), "1152921504606847000");
        assert_eq!(to_js_string(-1.7976931348623157e308), "-1.7976931348623157e+308");
        assert_eq!(to_js_string(5e-324), "5e-324");
        assert_eq!(to_js_string(-1.25e-300), "-1.25e-300");
        assert_eq!(to_js_string(-0.0000012345), "-0.0000012345");
    }

    /// An exact tie between two shortest candidates takes the even one, as
    /// V8 does and the specification recommends (TASK-103.59). Expected
    /// values are Node's.
    #[test]
    fn exact_ties_take_the_even_digit() {
        let cases: &[(f64, &str)] = &[
            (2f64.powi(-25), "2.9802322387695312e-8"),
            (-(2f64.powi(-25)), "-2.9802322387695312e-8"),
            (5.0 * 2f64.powi(-23), "5.960464477539062e-7"),
            (9.0 * 2f64.powi(-23), "0.0000010728836059570312"),
            // 2^-24 is a power of two: the even neighbour, ...062, is closer
            // to the double below, so the odd ...063 is the only choice.
            (2f64.powi(-24), "5.960464477539063e-8"),
            (2f64.powi(-21), "4.76837158203125e-7"),
            (2059402675471610.2, "2059402675471610.2"),
            (-2221614920546122.2, "-2221614920546122.2"),
            (901134.0629882812, "901134.0629882812"),
            (-638993.1938476562, "-638993.1938476562"),
            (0.023691177368164062, "0.023691177368164062"),
            (1173.6775512695312, "1173.6775512695312"),
            (-120696.92993164062, "-120696.92993164062"),
            (0.00008440017700195312, "0.00008440017700195312"),
        ];
        for &(x, want) in cases {
            assert_eq!(to_js_string(x), want, "{x:e} ({:#x})", x.to_bits());
        }
    }

    #[test]
    fn agrees_with_the_reference_on_many_values() {
        let mut edge = vec![
            f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -0.0, f64::MIN_POSITIVE, f64::MAX, f64::MIN,
            f64::EPSILON, 1e21, 1e21 - 65536.0, 999999999999999900000.0, 1e-6, 1e-7, 0.25, 0.5, 0.75,
        ];
        for i in -400..400 {
            edge.push(10f64.powi(i));
            edge.push(-1.5 * 10f64.powi(i));
            edge.push(2f64.powi(i));
            edge.push(i as f64 / 4.0);
        }
        // Random bit patterns (an LCG) cover every exponent and mantissa.
        let mut s: u64 = 0x9E3779B97F4A7C15;
        // Odd m times 2^-q: exact decimals of at most 18 digits, the only
        // values with a tie between two shortest candidates.
        for _ in 0..100_000 {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let q = (s % 30) as i32;
            let m = ((s >> 11) >> (s >> 58)) | 1;
            edge.push(m as f64 * 2f64.powi(-q));
        }
        for _ in 0..200_000 {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            edge.push(f64::from_bits(s));
            edge.push((s >> 11) as f64);
            edge.push((s >> 40) as f64 / 1000.0);
        }
        for x in edge {
            assert_eq!(to_js_string(x), reference(x), "{x:e} ({:#x})", x.to_bits());
        }
    }
}
