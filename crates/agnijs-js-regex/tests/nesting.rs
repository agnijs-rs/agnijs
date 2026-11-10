//! Deeply nested patterns (TASK-100.34). Every pass over a pattern (the
//! grammar check, the parser, both compilers and the analyses between them)
//! follows its nesting, so a pattern nested past
//! [`syntax::MAX_NESTING`] is a `SyntaxError`, and one nested up to it
//! compiles and matches, all on the default 2 MB thread stack. Programs
//! loaded from bytes are flat and verified with explicit stacks, so even a
//! program nested far deeper than any pattern may be loads and runs.

use agnijs_js_regex::syntax::{self, MAX_NESTING};
use agnijs_js_regex::{Flags, MatcherMode, Regex};

const MODES: [MatcherMode; 3] = [MatcherMode::BacktrackingOnly, MatcherMode::LinearWhenEligible, MatcherMode::Auto];

/// Far past the limit: deep enough to overflow any stack a recursive pass
/// could run on.
const DEEP: usize = 100_000;

/// Run `f` on a thread with the default 2 MB stack (set explicitly, so
/// `RUST_MIN_STACK` cannot hide an overflow).
fn on_default_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

fn u(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn flags(f: &str) -> Flags {
    Flags::parse(&u(f)).unwrap()
}

/// The ways one level of nesting can be written, as (open, close, flags).
/// Each adds one level, and a pattern matches what the same pattern
/// nested once does.
const KINDS: &[(&str, &str, &str)] = &[
    ("(", ")", ""),
    ("(?:", ")", ""),
    ("(?:", ")*", ""),
    ("(", ")+?", "u"),
    ("(?:b|", ")", ""),
    ("(?i:", ")", ""),
    ("(?=", ")", ""),
    ("(?<=", ")", ""),
    ("(?!b", ")", ""),
    ("(?<!b", ")", "u"),
    ("(?=", ")*", ""),
    ("[\\w&&", "]", "v"),
    ("[", "--b]", "v"),
];

fn nested(open: &str, close: &str, depth: usize, inner: &str) -> String {
    format!("{}{inner}{}", open.repeat(depth), close.repeat(depth))
}

/// Named groups need distinct names.
fn nested_named(depth: usize) -> String {
    let mut s: String = (0..depth).map(|i| format!("(?<g{i}>")).collect();
    s.push('a');
    s.push_str(&")".repeat(depth));
    s
}

/// A v-mode class nested `depth` deep: `[[[a]]]`.
fn nested_class(depth: usize) -> String {
    nested("[", "]", depth, "a")
}

fn deep_patterns(depth: usize) -> Vec<(String, &'static str)> {
    let mut v: Vec<(String, &str)> = KINDS.iter().map(|&(o, c, f)| (nested(o, c, depth, "a"), f)).collect();
    v.extend([(nested_named(depth), ""), (nested_class(depth), "v")]);
    v
}

#[test]
fn patterns_nested_far_past_the_limit_are_syntax_errors() {
    on_default_stack(|| {
        for (pattern, f) in deep_patterns(DEEP) {
            let head: String = pattern.chars().take(12).collect();
            assert!(syntax::validate(&pattern, f).is_err(), "validate /{head}.../{f}");
            let err = Regex::compile(&u(&pattern), &flags(f)).err();
            let err = err.unwrap_or_else(|| panic!("/{head}.../{f} compiled"));
            assert!(err.contains("nested too deeply"), "/{head}.../{f}: {err}");
        }
    });
}

#[test]
fn unbalanced_deep_nesting_is_still_a_syntax_error() {
    on_default_stack(|| {
        for (pattern, f) in [("(".repeat(DEEP), ""), ("(?:".repeat(DEEP) + "a)", ""), ("[".repeat(DEEP), "v")] {
            assert!(syntax::validate(&pattern, f).is_err());
            assert!(Regex::compile(&u(&pattern), &flags(f)).is_err());
        }
    });
}

#[test]
fn nesting_beyond_the_limit_counts_every_kind_together() {
    on_default_stack(|| {
        // Half groups, half lookarounds, then a class: one past the limit
        // in total although no kind alone reaches it.
        let half = MAX_NESTING / 2;
        let rest = MAX_NESTING - half;
        let pattern = format!("{}{}[[a]]{}{}", "(?:".repeat(half), "(?=".repeat(rest - 1), ")".repeat(rest - 1), ")".repeat(half));
        assert!(syntax::validate(&pattern, "v").is_err());
        assert!(Regex::compile(&u(&pattern), &flags("v")).is_err());
        let pattern = format!("{}{}[a]{}{}", "(?:".repeat(half), "(?=".repeat(rest - 1), ")".repeat(rest - 1), ")".repeat(half));
        assert!(syntax::validate(&pattern, "v").is_ok());
        assert!(Regex::compile(&u(&pattern), &flags("v")).is_ok());
    });
}

#[test]
fn patterns_nested_to_the_limit_compile_match_and_round_trip() {
    on_default_stack(|| {
        let input = u("xa");
        for ((pattern, f), (shallow, _)) in deep_patterns(MAX_NESTING).into_iter().zip(deep_patterns(1)) {
            let expected = Regex::compile_with(&u(&shallow), &flags(f), MatcherMode::BacktrackingOnly).unwrap().exec(&input, 0).unwrap().ranges[0];
            let head: String = pattern.chars().take(12).collect();
            assert!(syntax::validate(&pattern, f).is_ok(), "validate /{head}.../{f}");
            for mode in MODES {
                let re = Regex::compile_with(&u(&pattern), &flags(f), mode)
                    .unwrap_or_else(|e| panic!("/{head}.../{f} {mode:?}: {e}"));
                let m = re.exec(&input, 0).unwrap_or_else(|| panic!("/{head}.../{f} {mode:?} no match"));
                assert_eq!(m.ranges[0], expected, "/{head}.../{f} {mode:?}");
                let loaded = Regex::from_program_bytes(&re.program_bytes(), mode).unwrap();
                assert_eq!(loaded.exec(&input, 0), Some(m), "/{head}.../{f} {mode:?} loaded");
            }
        }
    });
}

#[test]
fn one_level_past_the_limit_is_a_syntax_error_for_every_kind() {
    on_default_stack(|| {
        for (pattern, f) in deep_patterns(MAX_NESTING + 1) {
            let head: String = pattern.chars().take(12).collect();
            assert!(syntax::validate(&pattern, f).is_err(), "validate /{head}.../{f}");
            assert!(Regex::compile(&u(&pattern), &flags(f)).is_err(), "/{head}.../{f}");
        }
    });
}

#[test]
fn captures_nested_to_the_limit_all_report_the_match() {
    on_default_stack(|| {
        let pattern = nested("(", ")", MAX_NESTING, "a");
        for mode in MODES {
            let re = Regex::compile_with(&u(&pattern), &Flags::default(), mode).unwrap();
            assert_eq!(re.group_count(), MAX_NESTING);
            let m = re.exec(&u("ba"), 0).unwrap();
            assert!(m.ranges.iter().all(|r| *r == Some((1, 2))), "{mode:?}");
        }
    });
}

/// Builds the bytes of a backtracking program of `depth` nested
/// lookaheads around nothing (the layout in `src/program.rs`): no names,
/// no sets, literals, loops or references, and no linear program.
fn nested_lookahead_program(depth: usize) -> Vec<u8> {
    fn uint(out: &mut Vec<u8>, mut v: u64) {
        loop {
            let b = (v & 0x7F) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }
    let mut b = vec![1]; // version
    uint(&mut b, 0); // names
    b.push(0); // starts_anchored
    b.push(0); // unicode
    uint(&mut b, 0); // groups
    uint(&mut b, 0); // first: none
    b.push(0); // skip_runs
    uint(&mut b, 0); // sets
    uint(&mut b, 0); // lits
    uint(&mut b, 0); // loops
    uint(&mut b, 0); // refs
    uint(&mut b, 2 * depth as u64 + 1); // insts
    for i in 0..depth {
        b.push(15); // Look { negate: false, end }
        b.push(0);
        uint(&mut b, (2 * depth - i) as u64);
    }
    b.extend(std::iter::repeat_n(16, depth)); // LookEnd
    b.push(18); // Match
    b.push(0); // linear: ineligible
    uint(&mut b, 3); // "lookaround"
    b
}

#[test]
fn programs_from_bytes_nested_far_past_the_limit_load_and_run() {
    on_default_stack(|| {
        let bytes = nested_lookahead_program(DEEP);
        for mode in MODES {
            let re = Regex::from_program_bytes(&bytes, mode).unwrap();
            assert_eq!(re.exec(&u("ab"), 1).unwrap().ranges, vec![Some((1, 1))]);
            // The encoding keeps the linear ineligibility reason only when
            // the linear matcher is built; without it every program is
            // written as "compiled out", so there the round trip is checked
            // on the re-encoded bytes instead.
            let out = re.program_bytes();
            if cfg!(feature = "linear") {
                assert_eq!(out, bytes);
            } else {
                let again = Regex::from_program_bytes(&out, mode).unwrap();
                assert_eq!(again.program_bytes(), out);
                assert_eq!(again.exec(&u("ab"), 1).unwrap().ranges, vec![Some((1, 1))]);
            }
        }
    });
}

