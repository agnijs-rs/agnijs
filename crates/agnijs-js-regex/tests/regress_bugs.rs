//! The divergences the differential fuzzer found in regress (TASK-105 to
//! TASK-108), which the engine's own matchers get right in every matcher
//! mode (TASK-100.31).

use agnijs_js_regex::{Flags, MatcherMode, Regex};

const MODES: [MatcherMode; 3] = [MatcherMode::BacktrackingOnly, MatcherMode::LinearWhenEligible, MatcherMode::Auto];

fn u(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// The whole-match range of /pattern/flags searched in `input` from 0,
/// the same in every matcher mode.
fn find(pattern: &str, flags: &str, input: &[u16]) -> Option<(usize, usize)> {
    let flags = Flags::parse(&u(flags)).unwrap();
    let results: Vec<_> = MODES
        .iter()
        .map(|&mode| {
            let re = Regex::compile_with(&u(pattern), &flags, mode).unwrap();
            re.exec(input, 0).map(|c| c.ranges[0].unwrap())
        })
        .collect();
    assert!(results.windows(2).all(|w| w[0] == w[1]), "/{pattern}/: {results:?}");
    results[0]
}

#[test]
fn escaped_surrogate_pairs_outside_u_mode_are_two_units() {
    // TASK-105
    assert_eq!(find(r"\uD83D\uDE00", "", &[0xD83D, 0xDE00]), Some((0, 2)));
    assert_eq!(find(r"\uD83Db?", "", &[0x61, 0xD83D, 0xDE00]), Some((1, 2)));
}

#[test]
fn braced_u_escape_outside_u_mode_is_an_identity_escape_and_a_quantifier() {
    // TASK-106: `\u{2}` is `u` twice.
    assert_eq!(find(r"^\u{2}$", "", &u("uu")), Some((0, 2)));
    assert_eq!(find(r"\u{2}", "", &[0x2]), None);
}

#[test]
fn word_boundaries_use_folded_word_characters_under_unicode_ignore_case() {
    // TASK-107: U+017F and U+212A fold to s and k, so they are word
    // characters under i with u or v, and there is no boundary after `a`.
    for flags in ["ui", "vi"] {
        assert_eq!(find(r"a\b", flags, &u("a\u{17F}")), None, "{flags}");
        assert_eq!(find(r"a\B", flags, &u("a\u{212A}")), Some((0, 1)), "{flags}");
    }
    assert_eq!(find(r"a\b", "u", &u("a\u{17F}")), Some((0, 1)));
}

// The rest were found by the differential test against regress
// (TASK-100.31) and have no tickets: regress is no longer the matcher.

#[test]
fn long_s_and_kelvin_sign_fold_only_to_themselves_outside_u_mode() {
    // Canonicalize maps U+017F to S, which is ASCII, so it keeps U+017F;
    // U+212A is uppercase already.
    assert_eq!(find("s", "i", &u("\u{17F}")), None);
    assert_eq!(find(r"\w", "i", &u("\u{212A}")), None);
    assert_eq!(find(r"\W", "i", &u("\u{212A}")), Some((0, 1)));
    assert_eq!(find("s", "iu", &u("\u{17F}")), Some((0, 1)));
}

#[test]
fn a_class_with_non_word_characters_under_ui_excludes_what_folds_to_word_characters() {
    assert_eq!(find(r"[\W]", "ui", &u("s")), None);
    assert_eq!(find(r"[\W]", "ui", &u("\u{17F}")), None);
}

#[test]
fn v_mode_under_i_folds_every_operand_before_set_operations() {
    assert_eq!(find("\u{212A}", "vi", &u("k")), Some((0, 1)));
    assert_eq!(find(r"[\p{L}--[a-z]]", "vi", &u("c")), None);
    assert_eq!(find(r"[\p{L}--[a-z]]", "vi", &u("C")), None);
    assert_eq!(find(r"[\p{L}--[a-z]]", "v", &u("C")), Some((0, 1)));
}

#[test]
fn a_v_mode_class_reads_backslash_b_as_backspace() {
    assert_eq!(find(r"[\b]", "v", &u("b")), None);
    assert_eq!(find(r"[\b]", "v", &u("\u{8}")), Some((0, 1)));
}

#[test]
fn a_v_mode_class_with_the_empty_string_matches_empty() {
    assert_eq!(find(r"[\q{abc|a|}b]", "v", &u("x")), Some((0, 0)));
    assert_eq!(find(r"[\q{abc|a|}b]", "v", &u("abc")), Some((0, 3)));
}

#[test]
fn empty_iterations_inside_a_counted_loop_finish() {
    // TASK-108: regress never returned on "__".
    assert_eq!(find(r"(?:(_|)+){2}K", "", &u("__")), None);
    assert_eq!(find(r"(?:(_|)+){2}K", "", &u("__K")), Some((0, 3)));
}
