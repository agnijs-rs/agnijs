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
    // Shortest round-tripping digits: Rust's `{:e}` gives them.
    let mut sci = [0u8; NUMBER_BUF_LEN];
    let mut w = StackWriter { buf: &mut sci, len: 0 };
    core::fmt::write(&mut w, format_args!("{x:e}")).expect("an f64 in {:e} fits the buffer");
    let sci_len = w.len;
    let sci = &sci[..sci_len];
    let e_at = sci.iter().position(|&c| c == b'e').expect("{:e} has an exponent");
    let mut digits = [0u8; 20];
    let mut k = 0usize;
    for &c in &sci[..e_at] {
        if c != b'.' {
            digits[k] = c;
            k += 1;
        }
    }
    let digits = &digits[..k];
    let exp_text = core::str::from_utf8(&sci[e_at + 1..]).expect("ASCII");
    let exp: i32 = exp_text.parse().expect("{:e} exponent is an integer");
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
