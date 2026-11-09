//! agnijs-js-regex: regular expressions for the AgniJS engine.
//!
//! The `syntax` module validates regular expression literals at parse
//! time (ES2025 pattern grammar plus Annex B). [`Regex`] is the matcher
//! seam the VM codes against: compile a pattern under a set of flags, then
//! run it against UTF-16 code units from a start index.
//!
//! Behind the seam is the engine's own regex implementation (ADR 0013,
//! TASK-100.31): `parse` turns a validated pattern into a tree with every
//! compile-time decision made (names, modifiers, case folding, v-mode set
//! operations, Annex B), and two matchers compile from that tree: the
//! backtracking matcher (`backtrack`), which handles every pattern, and
//! the linear-time matcher (`linear`), for patterns without
//! backreferences or lookaround. [`MatcherMode`] picks between them.

// ADR 0019 J10 (TASK-100.32): engine crates stay free of `unsafe`.
#![forbid(unsafe_code)]

pub mod syntax;

mod backtrack;
mod charset;
mod parse;
mod program;
mod unicode;
#[rustfmt::skip]
#[allow(dead_code, clippy::type_complexity)]
mod unicode_tables;

#[cfg(feature = "linear")]
mod linear;

use std::cell::RefCell;
use std::sync::Arc;

/// How compiled patterns are matched (ADR 0013 addendum). The mode never
/// changes what matches, only which matcher finds it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MatcherMode {
    /// Patterns without backreferences or lookaround backtrack under a
    /// step budget proportional to what the linear-time matcher would
    /// spend, and finish on the linear-time matcher if it runs out;
    /// patterns on which backtracking can be exponential go straight to
    /// the linear-time matcher. Everything else backtracks.
    #[default]
    Auto,
    /// Every pattern backtracks, as in V8 and most other engines.
    BacktrackingOnly,
    /// Every eligible pattern runs on the linear-time matcher, whatever
    /// the input. For tests and benchmarks.
    LinearWhenEligible,
}

/// Under [`MatcherMode::Auto`], how many backtracking steps an attempt
/// may take per linear-matcher step it would otherwise cost (program size
/// times input length) before it hands over to the linear matcher, and
/// the least it may always take.
#[cfg(feature = "linear")]
const BACKTRACK_STEPS_PER_LINEAR_STEP: u64 = 16;
#[cfg(feature = "linear")]
const BACKTRACK_MIN_BUDGET: u64 = 1 << 12;

/// The eight ECMAScript flags, parsed and validated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    pub has_indices: bool,
    pub global: bool,
    pub ignore_case: bool,
    pub multiline: bool,
    pub dot_all: bool,
    pub unicode: bool,
    pub unicode_sets: bool,
    pub sticky: bool,
}

impl Flags {
    /// Parse a flags string as `RegExpInitialize` does: every character
    /// must be one of `dgimsuvy`, none may repeat, and `u` and `v` are
    /// mutually exclusive.
    pub fn parse(flags: &[u16]) -> Result<Flags, String> {
        let mut f = Flags::default();
        for &u in flags {
            let slot = match u {
                0x64 => &mut f.has_indices,  // d
                0x67 => &mut f.global,       // g
                0x69 => &mut f.ignore_case,  // i
                0x6D => &mut f.multiline,    // m
                0x73 => &mut f.dot_all,      // s
                0x75 => &mut f.unicode,      // u
                0x76 => &mut f.unicode_sets, // v
                0x79 => &mut f.sticky,       // y
                _ => {
                    let c = char::from_u32(u as u32).unwrap_or('\u{FFFD}');
                    return Err(format!("Invalid regular expression flags '{c}'"));
                }
            };
            if *slot {
                return Err("Invalid regular expression flags: duplicate flag".into());
            }
            *slot = true;
        }
        if f.unicode && f.unicode_sets {
            return Err("Invalid regular expression flags: 'u' and 'v' are exclusive".into());
        }
        Ok(f)
    }

    /// Pattern and input are sequences of code points rather than code
    /// units.
    pub fn unicode_mode(&self) -> bool {
        self.unicode || self.unicode_sets
    }

    /// The canonical flags string, in `dgimsuvy` order.
    #[allow(clippy::inherent_to_string)]
    pub fn to_string(&self) -> String {
        let mut s = String::new();
        for (on, c) in [
            (self.has_indices, 'd'),
            (self.global, 'g'),
            (self.ignore_case, 'i'),
            (self.multiline, 'm'),
            (self.dot_all, 's'),
            (self.unicode, 'u'),
            (self.unicode_sets, 'v'),
            (self.sticky, 'y'),
        ] {
            if on {
                s.push(c);
            }
        }
        s
    }
}

/// A compiled pattern, independent of the matcher mode: what bytecode
/// serializes (ADR 0010).
#[derive(Debug, PartialEq, Eq)]
struct Program {
    /// Capture-group names by group number (1-based groups at index
    /// `n - 1`); `None` for unnamed groups.
    names: Vec<Option<String>>,
    /// Every alternative begins with a `^` outside the m flag, so a match
    /// can only start at index 0 (see [`Regex::starts_anchored`]).
    starts_anchored: bool,
    backtrack: backtrack::Prog,
    /// The linear-time program, or why the pattern is not eligible.
    #[cfg(feature = "linear")]
    linear: Result<linear::Linear, &'static str>,
}

/// A compiled pattern under a matcher mode, with its working memory.
pub struct Regex {
    program: Arc<Program>,
    mode: MatcherMode,
    bt_scratch: RefCell<backtrack::Scratch>,
    #[cfg(feature = "linear")]
    lin_scratch: RefCell<Option<linear::Scratch>>,
}

/// One successful match: `ranges[0]` is the whole match, `ranges[n]` the
/// n-th capture (`None` when the group did not participate). All indices
/// are UTF-16 code-unit offsets into the input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Captures {
    pub ranges: Vec<Option<(usize, usize)>>,
}

/// Whether every path through `n` begins with a non-multiline `^`, looking
/// through groups but not through quantifiers or lookaround.
fn starts_anchored(n: &parse::Node) -> bool {
    use parse::{Assertion, Node};
    match n {
        Node::Assert(Assertion::Start { multiline: false }) => true,
        Node::Capture(_, b) => starts_anchored(b),
        Node::Concat(v) => v.first().is_some_and(starts_anchored),
        Node::Alt(v) => v.iter().all(starts_anchored),
        _ => false,
    }
}

impl Regex {
    /// Compile `pattern` (UTF-16 code units, as the source text or
    /// `ToString(pattern)` gives it) under `flags`. The error message is
    /// what the `SyntaxError` carries.
    pub fn compile(pattern: &[u16], flags: &Flags) -> Result<Regex, String> {
        Regex::compile_with(pattern, flags, MatcherMode::Auto)
    }

    /// [`Regex::compile`] under an explicit matcher mode.
    pub fn compile_with(pattern: &[u16], flags: &Flags, mode: MatcherMode) -> Result<Regex, String> {
        // The grammar check first, so literals and `new RegExp` report the
        // same errors; the parser then only sees valid patterns.
        syntax::validate_units(pattern, flags.unicode, flags.unicode_sets)?;
        let code_points = parse::pattern_code_points(pattern, flags.unicode_mode());
        let parsed = parse::parse(
            &code_points,
            flags.ignore_case,
            flags.multiline,
            flags.dot_all,
            flags.unicode,
            flags.unicode_sets,
        )?;
        let program = Program {
            starts_anchored: starts_anchored(&parsed.node),
            backtrack: backtrack::Prog::compile(&parsed),
            #[cfg(feature = "linear")]
            linear: linear::Linear::compile(&parsed),
            names: parsed.names,
        };
        Ok(Regex::from_program(Arc::new(program), mode))
    }

    /// The compiled program as bytes, for bytecode to embed (ADR 0010
    /// addendum). They do not depend on the matcher mode.
    pub fn program_bytes(&self) -> Vec<u8> {
        program::encode(&self.program)
    }

    /// A pattern from [`Regex::program_bytes`], without parsing or
    /// compiling it again. The bytes are verified first: anything that is
    /// not shaped like a program the compiler produces is an error, never
    /// a panic or a match that cannot finish.
    pub fn from_program_bytes(bytes: &[u8], mode: MatcherMode) -> Result<Regex, String> {
        let program = program::decode(bytes).map_err(|e| format!("Invalid compiled regular expression: {e}"))?;
        Ok(Regex::from_program(Arc::new(program), mode))
    }

    fn from_program(program: Arc<Program>, mode: MatcherMode) -> Regex {
        Regex {
            program,
            mode,
            bt_scratch: RefCell::new(backtrack::Scratch::default()),
            #[cfg(feature = "linear")]
            lin_scratch: RefCell::new(None),
        }
    }

    /// Whether every alternative of the pattern begins with a `^` that the
    /// m flag does not reach, so that a match can only start at index 0.
    /// `exec` then tries that one position instead of every position
    /// (finding 9g: a failed `/^x/` was linear in the input). The test is
    /// conservative: it looks through capturing, non-capturing and
    /// modifier groups that do not add `m`, never through lookaround or a
    /// quantified group.
    pub fn starts_anchored(&self) -> bool {
        self.program.starts_anchored
    }

    /// The matcher mode this pattern was compiled under.
    pub fn mode(&self) -> MatcherMode {
        self.mode
    }

    /// Whether the pattern can run on the linear-time matcher, or why
    /// not. Ineligible patterns always backtrack.
    pub fn linear_eligibility(&self) -> Result<(), &'static str> {
        #[cfg(feature = "linear")]
        {
            if self.mode == MatcherMode::BacktrackingOnly {
                return Err("the matcher mode is backtracking only");
            }
            self.program.linear.as_ref().map(|_| ()).map_err(|e| *e)
        }
        #[cfg(not(feature = "linear"))]
        {
            Err("the linear matcher is compiled out")
        }
    }

    /// Whether a search over `input` from `start` begins on the linear-time
    /// matcher (the routing rule, exposed for tests). Under
    /// [`MatcherMode::Auto`] a search that begins on the backtracking
    /// matcher can still finish on the linear one when it runs out of
    /// steps.
    pub fn uses_linear_matcher(&self, input_len: usize, start: usize, sticky: bool) -> bool {
        let _ = (input_len, start, sticky);
        #[cfg(feature = "linear")]
        {
            matches!(self.route(input_len.saturating_sub(start)), Route::Linear(_))
        }
        #[cfg(not(feature = "linear"))]
        {
            false
        }
    }

    /// Number of capturing groups (not counting the whole match).
    pub fn group_count(&self) -> usize {
        self.program.names.len()
    }

    /// Capture-group names, index `i` for group `i + 1`.
    pub fn group_names(&self) -> &[Option<String>] {
        &self.program.names
    }

    /// Whether any group is named.
    pub fn has_named_groups(&self) -> bool {
        self.program.names.iter().any(|n| n.is_some())
    }

    /// Find the leftmost match starting at or after `start`.
    pub fn exec(&self, input: &[u16], start: usize) -> Option<Captures> {
        if self.program.starts_anchored {
            // Only index 0 can begin a match, so one exact-start attempt
            // there decides it; from anywhere else the answer is no.
            // Without this a failed `/^x/` tried every position.
            return if start == 0 { self.search(input, 0, true) } else { None };
        }
        self.search(input, start, false)
    }

    /// Find a match that starts exactly at `start` (sticky semantics).
    pub fn exec_sticky(&self, input: &[u16], start: usize) -> Option<Captures> {
        self.search(input, start, true)
    }

    fn search(&self, input: &[u16], start: usize, sticky: bool) -> Option<Captures> {
        if start > input.len() {
            return None;
        }
        #[cfg(feature = "linear")]
        let budget = match self.route(input.len() - start) {
            Route::Linear(lin) => return self.exec_linear(lin, input, start, sticky),
            Route::Backtrack(budget) => budget,
        };
        #[cfg(not(feature = "linear"))]
        let budget = u64::MAX;
        let outcome = match self.bt_scratch.try_borrow_mut() {
            Ok(mut s) => self.program.backtrack.exec(&mut s, input, start, sticky, budget),
            Err(_) => self.program.backtrack.exec(&mut Default::default(), input, start, sticky, budget),
        };
        match outcome {
            backtrack::Outcome::Match(ranges) => well_formed(ranges),
            backtrack::Outcome::NoMatch => None,
            backtrack::Outcome::Exhausted(_at) => {
                #[cfg(feature = "linear")]
                if let Ok(lin) = &self.program.linear {
                    // Every start before `_at` has failed, so the leftmost
                    // match from `_at` is the leftmost match from `start`.
                    return self.exec_linear(lin, input, _at, sticky);
                }
                unreachable!("only budgeted searches run out of steps")
            }
        }
    }

    #[cfg(feature = "linear")]
    fn route(&self, len: usize) -> Route<'_> {
        let Ok(lin) = &self.program.linear else { return Route::Backtrack(u64::MAX) };
        match self.mode {
            MatcherMode::BacktrackingOnly => Route::Backtrack(u64::MAX),
            MatcherMode::LinearWhenEligible => Route::Linear(lin),
            MatcherMode::Auto if lin.nested => Route::Linear(lin),
            MatcherMode::Auto => {
                let linear_cost = lin.size().saturating_mul(len as u64 + 1);
                Route::Backtrack(
                    linear_cost.saturating_mul(BACKTRACK_STEPS_PER_LINEAR_STEP).max(BACKTRACK_MIN_BUDGET),
                )
            }
        }
    }

    #[cfg(feature = "linear")]
    fn exec_linear(&self, lin: &linear::Linear, input: &[u16], start: usize, sticky: bool) -> Option<Captures> {
        let ranges = match self.lin_scratch.try_borrow_mut() {
            Ok(mut s) => {
                let s = s.get_or_insert_with(|| lin.scratch());
                lin.exec(s, input, start, sticky)
            }
            Err(_) => lin.exec(&mut lin.scratch(), input, start, sticky),
        }?;
        well_formed(ranges)
    }
}

/// The match, which callers may slice the input with. A compiled program
/// always gives a whole match and ranges that do not run backwards; this
/// keeps a program from bytes to the same promise (a verified one that
/// records captures oddly may match wrongly, but cannot make a caller
/// panic).
fn well_formed(ranges: Vec<Option<(usize, usize)>>) -> Option<Captures> {
    let ok = ranges.first().is_some_and(|r| r.is_some()) && ranges.iter().flatten().all(|&(a, b)| a <= b);
    ok.then_some(Captures { ranges })
}

#[cfg(feature = "linear")]
enum Route<'a> {
    Linear(&'a linear::Linear),
    /// Backtrack with this many steps.
    Backtrack(u64),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn flags_parse_and_print() {
        let f = Flags::parse(&u("gimsuyd")).unwrap();
        assert_eq!(f.to_string(), "dgimsuy");
        assert!(Flags::parse(&u("gg")).is_err());
        assert!(Flags::parse(&u("uv")).is_err());
        assert!(Flags::parse(&u("x")).is_err());
    }

    #[test]
    fn matches_report_code_unit_ranges() {
        let re = Regex::compile(&u("(a)(b)?"), &Flags::default()).unwrap();
        let c = re.exec(&u("xxab"), 0).unwrap();
        assert_eq!(c.ranges, vec![Some((2, 4)), Some((2, 3)), Some((3, 4))]);
        let c = re.exec(&u("xa"), 0).unwrap();
        assert_eq!(c.ranges, vec![Some((1, 2)), Some((1, 2)), None]);
        assert!(re.exec_sticky(&u("xa"), 0).is_none());
        assert!(re.exec_sticky(&u("xa"), 1).is_some());
    }

    #[test]
    fn sticky_matches_keep_groups_alternatives_and_empty_matches() {
        let re = Regex::compile(&u("(a)|b(c)?"), &Flags::default()).unwrap();
        assert_eq!(re.exec_sticky(&u("xbc"), 1).unwrap().ranges, vec![Some((1, 3)), None, Some((2, 3))]);
        assert_eq!(re.exec_sticky(&u("xa"), 1).unwrap().ranges, vec![Some((1, 2)), Some((1, 2)), None]);
        assert!(re.exec_sticky(&u("xa"), 0).is_none());
        // A pattern that matches the empty string matches at `start`.
        let re = Regex::compile(&u("a*"), &Flags::default()).unwrap();
        assert_eq!(re.exec_sticky(&u("baa"), 0).unwrap().ranges, vec![Some((0, 0))]);
        assert_eq!(re.exec_sticky(&u("baa"), 1).unwrap().ranges, vec![Some((1, 3))]);
        // Backreferences, lookbehind and named groups see the original numbering.
        let re = Regex::compile(&u("(?<=x)(?<q>['\"]).*?\\k<q>"), &Flags::default()).unwrap();
        assert_eq!(re.exec_sticky(&u("x'a'"), 1).unwrap().ranges, vec![Some((1, 4)), Some((1, 2))]);
        assert!(re.exec_sticky(&u("y'a'"), 1).is_none());
        let re = Regex::compile(&u("."), &Flags { unicode: true, ..Flags::default() }).unwrap();
        assert_eq!(re.exec_sticky(&u("a\u{1F600}"), 1).unwrap().ranges, vec![Some((1, 3))]);
        assert!(re.exec_sticky(&u("a"), 1).is_none());
    }

    #[test]
    fn sticky_matching_keeps_escapes_that_name_no_group() {
        // `\2` and `\8` with one group are an octal and an identity escape
        // (Annex B), not backreferences.
        let bt = |p: &str| Regex::compile_with(&u(p), &Flags::default(), MatcherMode::BacktrackingOnly).unwrap();
        let re = bt("(a)\\8");
        assert!(re.exec_sticky(&u("a"), 0).is_none());
        assert_eq!(re.exec_sticky(&u("a8"), 0).unwrap().ranges, vec![Some((0, 2)), Some((0, 1))]);
        let re = bt("(a)\\2");
        assert!(re.exec_sticky(&u("a"), 0).is_none());
        assert_eq!(re.exec_sticky(&u("a\u{2}"), 0).unwrap().ranges, vec![Some((0, 2)), Some((0, 1))]);
        let re = bt("(a)\\24[\\2]");
        assert_eq!(re.exec_sticky(&u("a\u{14}\u{2}"), 0).unwrap().ranges, vec![Some((0, 3)), Some((0, 1))]);
        let re = bt("()()()()()()()()\\9");
        assert!(re.exec_sticky(&u(""), 0).is_none());
        assert_eq!(re.exec_sticky(&u("9"), 0).unwrap().ranges[0], Some((0, 1)));
        let re = bt("(a)\\2(?:)7");
        assert_eq!(re.exec_sticky(&u("a\u{2}7"), 0).unwrap().ranges[0], Some((0, 3)));
    }

    #[test]
    fn a_failed_sticky_match_does_not_search_the_rest_of_the_input() {
        // A tokenizer probes `/[a-z]+/y` at every position of a long line;
        // each failed probe must cost O(1), not O(rest of the input).
        let input: Vec<u16> = "'".repeat(200_000).encode_utf16().collect();
        let re = Regex::compile(&u("[a-z]+"), &Flags { sticky: true, ..Flags::default() }).unwrap();
        let t = std::time::Instant::now();
        for i in 0..2_000 {
            assert!(re.exec_sticky(&input, i).is_none());
        }
        assert!(t.elapsed() < std::time::Duration::from_secs(5), "took {:?}", t.elapsed());
    }

    #[test]
    fn group_names_follow_group_order() {
        let re = Regex::compile(&u("(?<y>a)|(?<y>b)(c)(?:d)(?<=d)[(?<z>]"), &Flags::default()).unwrap();
        assert_eq!(re.group_names(), &[Some("y".into()), Some("y".into()), None]);
        let c = re.exec(&u("bcd("), 0).unwrap();
        assert_eq!(c.ranges[2], Some((0, 1)));
    }

    #[test]
    fn duplicate_named_groups_backreference_the_participating_group() {
        let re = Regex::compile(&u("(?:(?<x>a)|(?<x>b))\\k<x>"), &Flags::default()).unwrap();
        assert_eq!(re.exec(&u("bb"), 0).unwrap().ranges[0], Some((0, 2)));
        assert_eq!(re.exec(&u("aa"), 0).unwrap().ranges[0], Some((0, 2)));
        assert!(re.exec(&u("ab"), 0).is_none());
        let re = Regex::compile(&u("(?:(?:(?<x>a)|(?<x>b))\\k<x>){2}"), &Flags::default()).unwrap();
        let c = re.exec(&u("aabb"), 0).unwrap();
        assert_eq!(c.ranges, vec![Some((0, 4)), None, Some((2, 3))]);
        // A digit after the reference stays a literal.
        let re = Regex::compile(&u("(?<n>a)\\k<n>1"), &Flags::default()).unwrap();
        assert!(re.exec(&u("aa1"), 0).is_some());
    }

    #[test]
    fn astral_group_names_work_without_the_u_flag() {
        let re = Regex::compile(&u("(?<𝑓𝑜>fox)"), &Flags::default()).unwrap();
        assert_eq!(re.group_names(), &[Some("𝑓𝑜".into())]);
        assert!(re.exec(&u("a fox"), 0).is_some());
    }

    #[test]
    fn unicode_mode_sees_code_points() {
        let re = Regex::compile(&u("^.$"), &Flags { unicode: true, ..Flags::default() }).unwrap();
        assert!(re.exec(&u("\u{1F600}"), 0).is_some());
        let re = Regex::compile(&u("^.$"), &Flags::default()).unwrap();
        assert!(re.exec(&u("\u{1F600}"), 0).is_none());
    }

    #[test]
    fn syntax_errors_are_reported() {
        assert!(Regex::compile(&u("("), &Flags::default()).is_err());
        assert!(Regex::compile(&u("\\p{Foo}"), &Flags { unicode: true, ..Flags::default() }).is_err());
    }
}
