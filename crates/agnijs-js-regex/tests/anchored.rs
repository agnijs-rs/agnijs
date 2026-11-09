//! A pattern whose every alternative begins with a non-multiline `^` can
//! only match at index 0, so a failed `exec` must not scan the input
//! (TASK-100.30). Both matchers, and the routing between them, are covered.

use agnijs_js_regex::{Flags, MatcherMode, Regex};
use std::time::{Duration, Instant};

fn u(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn modes() -> Vec<MatcherMode> {
    let mut m = vec![MatcherMode::Auto, MatcherMode::BacktrackingOnly];
    if cfg!(feature = "linear") {
        m.push(MatcherMode::LinearWhenEligible);
    }
    m
}

fn re(pattern: &str, flags: &str, mode: MatcherMode) -> Regex {
    Regex::compile_with(&u(pattern), &Flags::parse(&u(flags)).unwrap(), mode).unwrap()
}

#[test]
fn a_failed_exec_of_an_anchored_pattern_does_not_scan_the_input() {
    // The ticket's reproduction: /^x/ over a million characters, 200 times.
    // Linear in the input this took 0.9 s in a release build (several
    // seconds in a debug one); constant time is microseconds.
    let input = u(&"a".repeat(1_000_000));
    for mode in modes() {
        for flags in ["", "g", "i", "s", "u", "v"] {
            let r = re("^x", flags, mode);
            let t = Instant::now();
            for _ in 0..200 {
                assert!(r.exec(&input, 0).is_none(), "/^x/{flags} {mode:?}");
            }
            // A global regex whose lastIndex is past 0 fails at once too.
            for _ in 0..200 {
                assert!(r.exec(&input, 5).is_none(), "/^x/{flags} {mode:?} from 5");
            }
            let took = t.elapsed();
            assert!(took < Duration::from_millis(500), "/^x/{flags} {mode:?} took {took:?}");
        }
    }
    // Alternatives and groups that keep the anchor.
    for p in ["^x|^y", "(^x)", "(?:^x)", "(?i:^x)", "(?-m:^x)", "((?:^x)|(^y))", "^x*y", "^(?:x|y)+"] {
        for mode in modes() {
            let r = re(p, "", mode);
            let t = Instant::now();
            for _ in 0..100 {
                assert!(r.exec(&input, 0).is_none(), "/{p}/ {mode:?}");
            }
            assert!(t.elapsed() < Duration::from_millis(500), "/{p}/ {mode:?} took {:?}", t.elapsed());
        }
    }
}

#[test]
fn anchored_patterns_are_recognised_and_the_rest_are_not() {
    for (p, flags, anchored) in [
        ("^x", "", true),
        ("^x", "dgisuy", true),
        ("^x", "v", true),
        ("^x|^y", "", true),
        ("(^x)", "", true),
        ("(?:^x)", "", true),
        ("(?i:^x)", "", true),
        ("(?-m:^x)", "", true),
        ("((?:^x)|(^y))", "", true),
        ("^x*y", "", true),
        ("^", "", true),
        ("^$", "", true),
        // The m flag, or a modifier that adds it, makes `^` match after
        // every line terminator.
        ("^x", "m", false),
        ("(?m:^x)", "", false),
        ("(?im-s:^x)", "", false),
        // One alternative without the anchor.
        ("^x|y", "", false),
        ("^x|", "", false),
        ("x|^y", "", false),
        ("(^x|y)", "", false),
        // Lookaround: a lookbehind on `^` matches further on, and a
        // lookahead is left alone.
        ("(?<=^x)y", "", false),
        ("(?<!^x)y", "", false),
        ("(?=^x)x", "", false),
        ("(?!^x)y", "", false),
        // A quantified group may be skipped.
        ("(^x)*y", "", false),
        ("(?:^x)?y", "", false),
        ("(^x){1}y", "", false),
        // Not an anchor at all.
        ("\\^x", "", false),
        ("[^x]", "", false),
        ("x^", "", false),
        ("(?:)^x", "", false),
        ("", "", false),
    ] {
        for mode in modes() {
            assert_eq!(re(p, flags, mode).starts_anchored(), anchored, "/{}/{flags} {mode:?}", p.escape_debug());
        }
    }
}

#[test]
fn the_shortcut_keeps_exact_matching_semantics() {
    type Case = (&'static str, &'static str, &'static str, usize, Option<Vec<Option<(usize, usize)>>>);
    let cases: Vec<Case> = vec![
        ("^a", "", "a", 0, Some(vec![Some((0, 1))])),
        ("^a", "", "a", 1, None),
        ("^a", "", "ba", 0, None),
        ("^a|^b", "", "b", 0, Some(vec![Some((0, 1))])),
        ("^a|^b", "", "xb", 0, None),
        ("(^a)|(^b)", "", "b", 0, Some(vec![Some((0, 1)), None, Some((0, 1))])),
        ("(?:^a)(b)?", "", "a", 0, Some(vec![Some((0, 1)), None])),
        ("^", "", "abc", 0, Some(vec![Some((0, 0))])),
        ("^", "", "abc", 2, None),
        ("^a*", "", "aab", 0, Some(vec![Some((0, 2))])),
        ("^a*", "", "baa", 0, Some(vec![Some((0, 0))])),
        ("^a*", "", "baa", 1, None),
        // Not anchored: the search goes on past the start.
        ("^a|b", "", "xb", 0, Some(vec![Some((1, 2))])),
        ("^a|", "", "x", 1, Some(vec![Some((1, 1))])),
        ("(^a)*b", "", "xb", 0, Some(vec![Some((1, 2)), None])),
        ("(?:)^a", "", "a", 0, Some(vec![Some((0, 1))])),
        // Multiline, by flag or modifier.
        ("^a", "m", "x\na", 0, Some(vec![Some((2, 3))])),
        ("^a", "m", "x\na", 1, Some(vec![Some((2, 3))])),
        ("(?m:^a)", "", "x\na", 0, Some(vec![Some((2, 3))])),
        // Lookbehind on the anchor.
        ("(?<=^x)a", "", "xa", 0, Some(vec![Some((1, 2))])),
        ("(?<=x)^a", "", "xa", 0, None),
        ("(?<=x)^a", "m", "x\na", 0, None),
        // Case folding and unicode mode.
        ("^a", "i", "A", 0, Some(vec![Some((0, 1))])),
        ("^.", "u", "\u{1F600}", 0, Some(vec![Some((0, 2))])),
        ("^.", "", "\u{1F600}", 0, Some(vec![Some((0, 1))])),
        ("^\\p{L}+$", "u", "héllo", 0, Some(vec![Some((0, 5))])),
        ("^\\p{L}+$", "u", "héllo!", 0, None),
        // Backreferences keep their numbering through the attempt.
        ("^(a)\\1", "", "aa", 0, Some(vec![Some((0, 2)), Some((0, 1))])),
        ("^(a)\\1", "", "aba", 0, None),
        ("^(?<n>a)\\k<n>", "", "aa", 0, Some(vec![Some((0, 2)), Some((0, 1))])),
        ("^(a)\\2", "", "a\u{2}", 0, Some(vec![Some((0, 2)), Some((0, 1))])),
    ];
    for (p, flags, input, start, expected) in cases {
        for mode in modes() {
            let r = re(p, flags, mode);
            let got = r.exec(&u(input), start).map(|c| c.ranges);
            assert_eq!(got, expected, "/{}/{flags} on {input:?} from {start} ({mode:?})", p.escape_debug());
        }
    }
}
