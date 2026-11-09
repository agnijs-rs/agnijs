//! Pathological patterns finish in linear time under the default matcher
//! mode (ADR 0013 and its addendum).

// Both matchers are needed; without the `linear` feature there is only one.
#![cfg(feature = "linear")]

use gable_js_regex::{Flags, Regex};
use std::time::{Duration, Instant};

fn u(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// Run `pattern` over `input` and assert it finishes well inside `limit`
/// (the backtracking matcher takes seconds to hours on each of these).
fn fast(pattern: &str, flags: &str, input: &str, expect_match: bool) {
    let re = Regex::compile(&u(pattern), &Flags::parse(&u(flags)).unwrap()).unwrap();
    let input = u(input);
    let t = Instant::now();
    let found = re.exec(&input, 0);
    let took = t.elapsed();
    assert_eq!(found.is_some(), expect_match, "/{pattern}/{flags}");
    assert!(took < Duration::from_secs(2), "/{pattern}/{flags} took {took:?}");
}

#[test]
fn nested_quantifiers_do_not_backtrack_exponentially() {
    // 14.9 s on regress with 28 a's (ADR 0013); here with 5000.
    let s = format!("{}!", "a".repeat(5000));
    fast("(a+)+$", "", &s, false);
    fast("(a*)*$", "", &s, true); // the empty match at the end
    fast("(a*)*!x", "", &s, false);
    fast("(a|aa)+$", "", &s, false);
    fast("(a|a?)+!x", "", &s, false);
    fast("^(\\w+\\s?)*$", "", &s, false);
    fast("(x+x+)+y", "", &"x".repeat(5000), false);
    fast("(a+)+$", "i", &format!("{}!", "A".repeat(5000)), false);
    fast("(.*a){12}", "s", &"a".repeat(5000).replace('a', "b"), false);
}

#[test]
fn polynomial_patterns_stay_linear_on_long_inputs() {
    // Backtracking retries the scan from every start: quadratic.
    let spaces = format!("{}x", " ".repeat(200_000));
    fast("\\s+$", "", &spaces, false);
    fast("a+b", "", &"a".repeat(200_000), false);
    fast("^[\\s\\u200c]+|[\\s\\u200c]+$", "", &spaces, true);
}
