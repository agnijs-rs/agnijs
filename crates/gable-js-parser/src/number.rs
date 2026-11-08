//! `Number::toString` for literal property keys (`{ 1.5: x }` has the key
//! "1.5", `{ 1e21: x }` has "1e+21"). Shared with the VM later.

/// ECMAScript `Number::toString(x)` with radix 10.
pub fn to_js_string(x: f64) -> String {
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
        return format!("-{}", to_js_string(-x));
    }
    // Shortest round-tripping digits: Rust's `{:e}` gives them.
    let sci = format!("{x:e}");
    let (mantissa, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exp + 1; // position of the decimal point relative to digits
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

#[cfg(test)]
mod tests {
    use super::to_js_string;

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
        assert_eq!(to_js_string(123456789012345680000.0), "123456789012345680000");
    }
}
