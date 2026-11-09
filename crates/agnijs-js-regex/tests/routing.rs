//! Which matcher runs a pattern (ADR 0013 addendum): eligibility, the
//! auto cost model, and the embedder's opt-out.

use agnijs_js_regex::{Flags, MatcherMode, Regex};

fn u(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn re(pattern: &str, flags: &str, mode: MatcherMode) -> Regex {
    Regex::compile_with(&u(pattern), &Flags::parse(&u(flags)).unwrap(), mode).unwrap()
}

#[cfg(feature = "linear")]
#[test]
fn features_the_linear_matcher_cannot_run_fall_back_to_backtracking() {
    for (pattern, flags) in [
        (r"(a)\1", ""),
        (r"(?<n>a)\k<n>", ""),
        (r"a(?=b)", ""),
        (r"a(?!b)", ""),
        (r"(?<=a)b", ""),
        (r"(?<!a)b", ""),
        (r"[\q{abc}]", "v"),
        (r"\p{RGI_Emoji}", "v"),
        (r"\u{61}", ""),
        (r"\uD83D\uDE00", ""),
    ] {
        let r = re(pattern, flags, MatcherMode::Auto);
        assert!(r.linear_eligibility().is_err(), "/{}/{flags}", pattern.escape_debug());
        assert!(!r.uses_linear_matcher(1 << 20, 0, false), "/{pattern}/{flags}");
        // Still matches, on the backtracking matcher.
        let _ = r.exec(&u("ab"), 0);
    }
    for (pattern, flags) in [
        (r"(a+)+$", ""),
        (r"(?<x>a|b)*c", "dgimsuy"),
        (r"[\p{L}--[a-z]]", "v"),
        (r"\uD83D\uDE00", "u"),
        (r"\b\w+\b", "i"),
    ] {
        assert!(re(pattern, flags, MatcherMode::Auto).linear_eligibility().is_ok(), "/{pattern}/{flags}");
    }
}

#[cfg(feature = "linear")]
#[test]
fn auto_sends_only_patterns_that_can_blow_up_to_the_linear_matcher() {
    // Nested quantifiers: linear at any input size.
    for p in [r"(a+)+$", r"(a|aa)*b", r"^(\w+\s?)*$", r"(?:x*)*y"] {
        assert!(re(p, "", MatcherMode::Auto).uses_linear_matcher(10, 0, false), "{p}");
    }
    // Ordinary patterns stay on backtracking for ordinary inputs ...
    for p in [r"\d{4}-\d{2}-\d{2}", r"^[a-z]+@[a-z]+\.com$", r"foo|bar", r"\s+$"] {
        let r = re(p, "", MatcherMode::Auto);
        assert!(!r.uses_linear_matcher(100, 0, false), "{p}");
        assert!(!r.uses_linear_matcher(100, 0, true), "{p} sticky");
    }
    // ... and polynomial ones move to linear once the input is long.
    let r = re(r"\s+$", "", MatcherMode::Auto);
    assert!(r.uses_linear_matcher(1 << 20, 0, false));
    // Backtracking that is already linear in the input stays, however long
    // the input: one loop under `^`, or a sticky scan.
    let r = re(r"^\p{L}+$", "u", MatcherMode::Auto);
    assert!(!r.uses_linear_matcher(1 << 20, 0, false));
    assert!(!re(r"[a-z]+", "", MatcherMode::Auto).uses_linear_matcher(1 << 20, 0, true));
    // A bounded, loop-free pattern never needs it.
    assert!(!re("abc|abd", "", MatcherMode::Auto).uses_linear_matcher(1 << 24, 0, false));
}

#[test]
fn backtracking_only_never_uses_the_linear_matcher() {
    let r = re(r"(a+)+$", "", MatcherMode::BacktrackingOnly);
    assert_eq!(r.mode(), MatcherMode::BacktrackingOnly);
    assert!(r.linear_eligibility().is_err());
    assert!(!r.uses_linear_matcher(1 << 20, 0, false));
    assert!(r.exec(&u("aaaa"), 0).is_some());
}

#[cfg(feature = "linear")]
#[test]
fn linear_when_eligible_forces_every_eligible_pattern() {
    let r = re("abc", "", MatcherMode::LinearWhenEligible);
    assert!(r.uses_linear_matcher(3, 0, false));
    assert!(!re(r"(a)\1", "", MatcherMode::LinearWhenEligible).uses_linear_matcher(3, 0, false));
}

#[cfg(not(feature = "linear"))]
#[test]
fn compiled_out_linear_matcher_is_never_used() {
    let r = re(r"(a+)+$", "", MatcherMode::Auto);
    assert_eq!(r.linear_eligibility(), Err("the linear matcher is compiled out"));
    assert!(!r.uses_linear_matcher(1 << 20, 0, false));
}
