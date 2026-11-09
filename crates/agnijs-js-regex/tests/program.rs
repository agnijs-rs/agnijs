//! Compiled programs as bytes (ADR 0010 addendum, TASK-100.31): what
//! bytecode embeds for each regex literal, so loading it skips the parse
//! and the compile.

use agnijs_js_regex::{Captures, Flags, MatcherMode, Regex};

const MODES: [MatcherMode; 3] = [MatcherMode::BacktrackingOnly, MatcherMode::LinearWhenEligible, MatcherMode::Auto];

fn u(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn compile(pattern: &str, flags: &str, mode: MatcherMode) -> Regex {
    Regex::compile_with(&u(pattern), &Flags::parse(&u(flags)).unwrap(), mode).unwrap()
}

/// Every search and sticky attempt over `input`.
fn outcomes(re: &Regex, input: &[u16]) -> Vec<Option<Captures>> {
    (0..=input.len()).flat_map(|i| [re.exec(input, i), re.exec_sticky(input, i)]).collect()
}

/// Patterns that between them use every instruction of both matchers.
const PATTERNS: &[(&str, &str, &str)] = &[
    (r"(?<year>\d{4})-(?<month>\d\d)", "", "1999-12 and 2024-01"),
    (r"(a|ab)(c|bcd)(d*)", "", "abcd"),
    (r"^(?:a+|b)*?c$", "m", "ab\naabc\nc"),
    (r"(\w+)\s\1", "i", "the The end"),
    (r"(?<=\$)\d+(?!\d|px)", "", "cost $42 or $7px"),
    (r"(?<!a)b+", "y", "abbb bb"),
    (r"\bfoo\B.", "", "foo foox"),
    (r"(?:(_|)+){2}K", "", "__K"),
    (r"[\p{L}--[a-z]]+", "v", "abcDEF"),
    (r"[\q{abc|a|}b]", "v", "xabc"),
    (r"\u{1F600}+|.", "su", "\u{1F600}\u{1F600}\n"),
    (r"(?i:k)\w", "u", "\u{212A}x"),
    (r"a{2,4}?b{3}", "", "aaaabbb"),
    (r"(?:x(y)?)+z", "", "xyxz"),
    (r"(a)|(?<n>b)\k<n>", "d", "bb"),
];

#[test]
fn a_program_loaded_from_its_bytes_matches_like_the_compiled_one() {
    for &(pattern, flags, input) in PATTERNS {
        for mode in MODES {
            let re = compile(pattern, flags, mode);
            let loaded = Regex::from_program_bytes(&re.program_bytes(), mode).unwrap();
            let input = u(input);
            assert_eq!(outcomes(&loaded, &input), outcomes(&re, &input), "/{pattern}/{flags} {mode:?}");
            assert_eq!(loaded.group_names(), re.group_names());
            assert_eq!(loaded.starts_anchored(), re.starts_anchored());
            assert_eq!(loaded.linear_eligibility(), re.linear_eligibility());
            assert_eq!(loaded.mode(), mode);
            assert_eq!(loaded.program_bytes(), re.program_bytes(), "/{pattern}/{flags}: bytes round trip");
        }
    }
}

#[test]
fn the_bytes_do_not_depend_on_the_matcher_mode() {
    for &(pattern, flags, _) in PATTERNS {
        let bytes: Vec<_> = MODES.iter().map(|&m| compile(pattern, flags, m).program_bytes()).collect();
        assert!(bytes.windows(2).all(|w| w[0] == w[1]), "/{pattern}/{flags}");
    }
}

#[test]
fn truncated_or_extended_bytes_are_rejected() {
    for &(pattern, flags, _) in PATTERNS {
        let bytes = compile(pattern, flags, MatcherMode::Auto).program_bytes();
        for n in 0..bytes.len() {
            assert!(Regex::from_program_bytes(&bytes[..n], MatcherMode::Auto).is_err(), "/{pattern}/{flags} cut at {n}");
        }
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(Regex::from_program_bytes(&longer, MatcherMode::Auto).is_err(), "/{pattern}/{flags} with a trailing byte");
    }
}

#[test]
fn a_program_that_jumps_out_of_range_is_rejected() {
    // A one-instruction backtracking program that jumps past its end,
    // built by hand: version, no names, not anchored, then the
    // backtracking part (not unicode, 0 groups, no first set, no run skip,
    // no sets, lits, loops or refs, one Jmp to 5), then an ineligible
    // linear part (reason 0).
    let bytes = [1u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 4, 5, 0, 0];
    assert!(Regex::from_program_bytes(&bytes, MatcherMode::Auto).is_err());
    // The same program with a Match instead loads and matches empty.
    let bytes = [1u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 18, 0, 0];
    let re = Regex::from_program_bytes(&bytes, MatcherMode::Auto).unwrap();
    assert_eq!(re.exec(&u("ab"), 1).unwrap().ranges, vec![Some((1, 1))]);
}
