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
        // Both matchers compile from one parse tree (TASK-100.31), so
        // classes with strings and the escapes regress misread are
        // eligible too.
        (r"[\q{abc}]", "v"),
        (r"\p{RGI_Emoji}", "v"),
        (r"\u{61}", ""),
        (r"😀", ""),
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
    // ... and so do polynomial ones, however long the input: they begin on
    // backtracking with a step budget and finish on the linear matcher
    // only if it runs out (tests/redos.rs times them).
    for p in [r"\s+$", r"^\p{L}+$", r"[a-z]+", "abc|abd"] {
        assert!(!re(p, "u", MatcherMode::Auto).uses_linear_matcher(1 << 24, 0, false), "{p}");
    }
    // A repeated alternation whose branches begin with different
    // characters cannot blow up: each iteration's branch is decided by its
    // first character.
    for p in [r#""(?:[^"\\]|\\.)*""#, r"(?:a|b)*c", r"(?:ab|cd)+$"] {
        assert!(!re(p, "", MatcherMode::Auto).uses_linear_matcher(1 << 20, 0, false), "{p}");
    }
    for p in [r"(?:a|ab)*c", r"(?:a|\w)+$", r"(?:a+|b)*c"] {
        assert!(re(p, "", MatcherMode::Auto).uses_linear_matcher(10, 0, false), "{p}");
    }
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
