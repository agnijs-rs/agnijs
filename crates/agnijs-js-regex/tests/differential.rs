//! Differential fuzzing of the matchers (ADR 0013 addendum, TASK-100.31).
//!
//! Random patterns from an ECMAScript grammar subset (captures, named and
//! duplicate-named groups, modifiers, every quantifier form, assertions,
//! lookaround, backreferences, classes, escapes, Annex B forms, `u` and `v`
//! mode syntax) run over random short inputs under random flags. Each
//! pattern is matched from every start index, sticky from every index, and
//! through a global `lastIndex` loop that advances past empty matches the
//! way `RegExpBuiltinExec` and `AdvanceStringIndex` do. Any difference in
//! the match or in any capture fails the test with the reproduction.
//!
//! - [`matchers_agree`]: the backtracking matcher against the linear one
//!   (every eligible pattern) and against `Auto`, which mixes them under a
//!   step budget. A pattern anchored by `^` is also compared with the same
//!   pattern behind an empty `(?:)`, which disables the index-0 shortcut
//!   (TASK-100.30). Quantifiers nest three deep here: the backtracking
//!   matcher guards empty iterations, so `(?:(_|)+){2}K` finishes.
//! - [`matchers_agree_on_deeply_nested_patterns`]: the same comparison
//!   (plus a program loaded from its bytes) on patterns wrapped in groups,
//!   lookarounds and `v`-mode classes up to and past
//!   `syntax::MAX_NESTING`, on a 2 MB stack; past the limit every entry
//!   point must report a syntax error (TASK-100.34). It runs a tenth of the
//!   case count.
//! - [`backtracking_agrees_with_regress`]: the backtracking matcher against
//!   `regress`, the matcher it replaced, kept as a test-only reference
//!   (`tests/reference`). The generator keeps regress's limits here (two
//!   nested quantifiers, single-character atoms at the second level) since
//!   regress never finishes some deeper patterns (TASK-108), and
//!   [`regress_bug`] skips the patterns where regress is known to be wrong
//!   (TASK-105, TASK-106, TASK-107).
//!
//! `cargo test` (and so CI) runs [`MIN_CASES`] patterns per test, each on
//! [`INPUTS_PER_PATTERN`] inputs, with a fixed seed. `AGNIJS_REGEX_FUZZ_CASES`
//! changes the count, `AGNIJS_REGEX_FUZZ_SEED` picks another seed and
//! `AGNIJS_REGEX_FUZZ_TRACE` prints each pattern before it runs. Keep
//! heavier runs bounded (one process, a raised count, a timeout) rather
//! than looping locally. A watchdog aborts the run, naming the pattern,
//! when any single case hangs.

// Both matchers are needed; without the `linear` feature there is only one.
#![cfg(feature = "linear")]

mod reference;

use agnijs_js_regex::{Captures, Flags, MatcherMode, Regex};
use reference::Reference;

/// Patterns compared per test in CI, each against several inputs.
const MIN_CASES: usize = 10_000;
const INPUTS_PER_PATTERN: usize = 6;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Legacy,
    U,
    V,
}

struct Gen<'r> {
    rng: &'r mut Rng,
    mode: Mode,
    names: usize,
    /// Quantifiers enclosing the atom being generated.
    quantifiers: usize,
    /// How many quantifiers may nest.
    max_quantifiers: usize,
    /// Whether the innermost allowed quantifier applies to single
    /// characters only (regress's limit, see the module comment).
    simple_innermost: bool,
}

const LITERALS: &[&str] = &["a", "b", "c", "A", "B", " ", "1", "_", "\u{17F}", "\u{212A}", "\u{E9}", "k", "s"];
const ESCAPES: &[&str] = &[
    "\\d", "\\D", "\\w", "\\W", "\\s", "\\S", "\\n", "\\t", "\\x61", "\\u0062", "\\0", "\\.", "\\-",
    "\\/", "\\^", "\\$", "\\*", "\\(", "\\[", "\\|", "\\cJ", "\\f", "\\v",
];
const CLASSES: &[&str] = &[
    "[ab]", "[^a]", "[a-c]", "[^a-c]", "[\\s\\S]", "[\\w]", "[\\d_]", "[]", "[^]", "[A-Z]", "[a-zA-Z]",
    "[\\b]", "[.]", "[\\]a]", "[-a]", "[a-]", "[\\u00e9k]", "[\\x41-\\x43]", "[\\W]",
];
const LEGACY_ONLY: &[&str] = &[
    "{", "}", "]", "a{,2}", "\\c", "\\c1", "\\8", "\\01", "\\101", "\\k", "\\p", "\\a", "\\u{2}",
    "\\x", "\\u", "[\\c_]", "[\\c1]", "[\\d-z]", "\\uD83D", "\\uDE00", "\\uD83D\\uDE00", "\u{1F600}",
];
const UNICODE_ONLY: &[&str] = &[
    "\\u{1F600}", "\u{1F600}", "\\uD83D\\uDE00", "\\uD83D", "\\uDE00", "\\p{Lu}", "\\P{Lu}", "\\p{L}",
    "\\p{Script=Greek}", "[\\p{Ll}\\d]", "[^\\p{Lu}]", "[\\u{1F600}-\\u{1F64F}]", "\\p{ASCII}", "[😀a]",
];
const SETS_ONLY: &[&str] = &[
    "[\\w--[a]]", "[[a-c]&&[b-d]]", "[\\p{L}--[a-z]]", "[[ab][c]]", "[^[ab]]", "[\\d\\q{1}]", "[\\q{ab|c}]",
    "\\p{RGI_Emoji}", "[a&&a]", "[\\q{abc|a|}b]",
];
const ASSERTIONS: &[&str] = &["^", "$", "\\b", "\\B"];
const QUANTIFIERS: &[&str] = &["*", "+", "?", "{0,2}", "{2}", "{1,}", "{0,1}", "{2,3}", "{0}", "{1,2}"];

impl Gen<'_> {
    fn disjunction(&mut self, depth: usize) -> String {
        let mut s = self.alternative(depth);
        while self.rng.chance(if depth == 0 { 25 } else { 20 }) {
            s.push('|');
            s.push_str(&self.alternative(depth));
        }
        s
    }

    fn alternative(&mut self, depth: usize) -> String {
        let n = self.rng.below(4);
        (0..n).map(|_| self.term(depth)).collect()
    }

    fn term(&mut self, depth: usize) -> String {
        if self.rng.chance(10) {
            return self.rng.pick(ASSERTIONS).to_string();
        }
        let quantify = self.quantifiers < self.max_quantifiers && self.rng.chance(40);
        if quantify {
            self.quantifiers += 1;
        }
        let mut s = self.atom(depth);
        if quantify {
            self.quantifiers -= 1;
            s.push_str(self.rng.pick(QUANTIFIERS));
            if self.rng.chance(25) {
                s.push('?');
            }
        }
        s
    }

    fn atom(&mut self, depth: usize) -> String {
        let simple = self.simple_innermost && self.quantifiers >= self.max_quantifiers;
        let r = if simple { self.rng.below(60) } else { self.rng.below(100) };
        if r < 30 || depth >= 3 {
            return self.rng.pick(LITERALS).to_string();
        }
        if r < 40 {
            return self.rng.pick(ESCAPES).to_string();
        }
        if r < 48 {
            return self.rng.pick(CLASSES).to_string();
        }
        if r < 54 {
            return ".".to_string();
        }
        if r < 60 {
            return match self.mode {
                Mode::Legacy => self.rng.pick(LEGACY_ONLY),
                Mode::U => self.rng.pick(UNICODE_ONLY),
                Mode::V => {
                    if self.rng.chance(50) {
                        self.rng.pick(SETS_ONLY)
                    } else {
                        self.rng.pick(UNICODE_ONLY)
                    }
                }
            }
            .to_string();
        }
        let body = self.disjunction(depth + 1);
        match self.rng.below(8) {
            0..=2 => format!("({body})"),
            3 | 4 => format!("(?:{body})"),
            5 => {
                self.names += 1;
                // Reuse a name sometimes: duplicates are legal only in
                // different alternatives, so some of these fail to compile
                // and are skipped. `\k<name>` follows some of them.
                let n = if self.names > 1 && self.rng.chance(30) { self.names - 1 } else { self.names };
                if self.rng.chance(20) {
                    format!("(?<g{n}>{body})\\k<g{n}>")
                } else {
                    format!("(?<g{n}>{body})")
                }
            }
            6 => format!("(?{}:{body})", self.rng.pick(&["i", "-i", "m", "s", "-s", "i-m", "-ms"])),
            _ => match self.rng.below(7) {
                0 => format!("(?={body})"),
                1 => format!("(?!{body})"),
                2 => format!("(?<={body})"),
                3 => format!("(?<!{body})"),
                4 => format!("({body})\\1"),
                5 => format!("\\1({body})"),
                _ => format!("({body})"),
            },
        }
    }
}

const INPUT_CHARS: &[&str] = &[
    "a", "a", "b", "c", "A", "B", " ", "\n", "1", "_", "k", "K", "s", "\u{17F}", "\u{212A}", "\u{E9}",
    "\u{C9}", "\u{1F600}", "\r", ".", "-",
];

fn random_input(rng: &mut Rng) -> Vec<u16> {
    let n = rng.below(9);
    let mut out = Vec::new();
    for _ in 0..n {
        if rng.chance(4) {
            // A lone surrogate.
            out.push(if rng.chance(50) { 0xD83D } else { 0xDE00 });
        } else {
            out.extend(rng.pick(INPUT_CHARS).encode_utf16());
        }
    }
    out
}

fn random_flags(rng: &mut Rng, mode: Mode) -> String {
    let mut f = String::new();
    for (c, p) in [('d', 20), ('g', 40), ('i', 35), ('m', 30), ('s', 30), ('y', 25)] {
        if rng.chance(p) {
            f.push(c);
        }
    }
    match mode {
        Mode::Legacy => {}
        Mode::U => f.push('u'),
        Mode::V => f.push('v'),
    }
    f
}

/// The matching operations a fuzzed matcher offers.
trait Matcher {
    fn exec(&self, input: &[u16], start: usize) -> Option<Captures>;
    fn exec_sticky(&self, input: &[u16], start: usize) -> Option<Captures>;
}

impl Matcher for Regex {
    fn exec(&self, input: &[u16], start: usize) -> Option<Captures> {
        Regex::exec(self, input, start)
    }
    fn exec_sticky(&self, input: &[u16], start: usize) -> Option<Captures> {
        Regex::exec_sticky(self, input, start)
    }
}

impl Matcher for Reference {
    fn exec(&self, input: &[u16], start: usize) -> Option<Captures> {
        Reference::exec(self, input, start)
    }
    fn exec_sticky(&self, input: &[u16], start: usize) -> Option<Captures> {
        Reference::exec_sticky(self, input, start)
    }
}

/// Every observable outcome of matching `re` against `input`: a search
/// from every index, a sticky attempt at every index, and the global or
/// sticky `lastIndex` loop.
fn outcomes(re: &dyn Matcher, input: &[u16], flags: &Flags) -> Vec<Option<Captures>> {
    let mut out = Vec::new();
    let unicode = flags.unicode_mode();
    let mid_pair = |i: usize| {
        unicode
            && i > 0
            && i < input.len()
            && (0xDC00..0xE000).contains(&input[i])
            && (0xD800..0xDC00).contains(&input[i - 1])
    };
    for start in 0..=input.len() {
        // The VM never starts a unicode-mode search inside a surrogate
        // pair (it moves back to the pair), and the seam does not define
        // that case.
        if mid_pair(start) {
            continue;
        }
        out.push(re.exec(input, start));
        out.push(re.exec_sticky(input, start));
    }
    // RegExpBuiltinExec under `g` / `y`, with AdvanceStringIndex on empty
    // matches and the unicode-mode start alignment the VM applies.
    let mut last = 0usize;
    for _ in 0..input.len() + 2 {
        if last > input.len() {
            break;
        }
        let attempt = if mid_pair(last) { last - 1 } else { last };
        let m = if flags.sticky { re.exec_sticky(input, attempt) } else { re.exec(input, attempt) };
        let Some(c) = m else {
            out.push(None);
            break;
        };
        let (s, e) = c.ranges[0].unwrap();
        out.push(Some(c));
        last = if s == e {
            let pair = unicode
                && e + 1 < input.len()
                && (0xD800..0xDC00).contains(&input[e])
                && (0xDC00..0xE000).contains(&input[e + 1]);
            e + if pair { 2 } else { 1 }
        } else {
            e
        };
        if !flags.global && !flags.sticky {
            break;
        }
    }
    out
}

fn show(units: &[u16]) -> String {
    String::from_utf16_lossy(units).escape_debug().to_string()
}

/// Aborts the run, naming the pattern, if one case takes over
/// [`Watchdog::LIMIT`]: a hang in any matcher must fail fast rather than
/// hold a CPU.
struct Watchdog {
    state: std::sync::Arc<std::sync::Mutex<(u64, String)>>,
}

impl Watchdog {
    const LIMIT: std::time::Duration = std::time::Duration::from_secs(20);

    fn start() -> Watchdog {
        let state = std::sync::Arc::new(std::sync::Mutex::new((0u64, String::new())));
        let s = std::sync::Arc::downgrade(&state);
        std::thread::spawn(move || {
            let mut last = (u64::MAX, std::time::Instant::now());
            loop {
                std::thread::sleep(std::time::Duration::from_millis(500));
                // The test finished and dropped its watchdog.
                let Some(s) = s.upgrade() else { return };
                let (n, p) = s.lock().unwrap().clone();
                if n != last.0 {
                    last = (n, std::time::Instant::now());
                } else if last.1.elapsed() > Watchdog::LIMIT {
                    eprintln!("case {n} did not finish in {:?}: {p}", Watchdog::LIMIT);
                    std::process::abort();
                }
            }
        });
        Watchdog { state }
    }

    fn case(&self, pattern: &str, flags: &str) {
        let mut g = self.state.lock().unwrap();
        g.0 += 1;
        g.1 = format!("/{}/{flags}", pattern.escape_debug());
    }
}

/// The run's settings from the environment (see the module comment).
fn settings() -> (usize, Rng, bool) {
    let cases: usize = std::env::var("AGNIJS_REGEX_FUZZ_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(MIN_CASES);
    let seed: u64 = std::env::var("AGNIJS_REGEX_FUZZ_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x5EED_0013);
    let trace = std::env::var_os("AGNIJS_REGEX_FUZZ_TRACE").is_some();
    (cases, Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1), trace)
}

/// One random pattern and flags string.
fn random_case(rng: &mut Rng, max_quantifiers: usize, simple_innermost: bool) -> (String, String) {
    let mode = match rng.below(10) {
        0..=5 => Mode::Legacy,
        6..=7 => Mode::U,
        _ => Mode::V,
    };
    let pattern = Gen { rng, mode, names: 0, quantifiers: 0, max_quantifiers, simple_innermost }.disjunction(0);
    (pattern, random_flags(rng, mode))
}

fn u(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

#[test]
fn matchers_agree() {
    let (cases, mut rng, trace) = settings();
    let watchdog = Watchdog::start();
    let (mut valid, mut eligible, mut invalid, mut comparisons, mut anchored) = (0usize, 0usize, 0usize, 0usize, 0usize);
    while valid < cases {
        let (pattern, flags_s) = random_case(&mut rng, 3, false);
        let p = u(&pattern);
        let flags = Flags::parse(&u(&flags_s)).unwrap();
        watchdog.case(&pattern, &flags_s);
        if trace {
            eprintln!("/{}/{flags_s}", pattern.escape_debug());
        }
        let Ok(back) = Regex::compile_with(&p, &flags, MatcherMode::BacktrackingOnly) else {
            invalid += 1;
            continue;
        };
        valid += 1;
        let lin = Regex::compile_with(&p, &flags, MatcherMode::LinearWhenEligible).unwrap();
        let auto = Regex::compile_with(&p, &flags, MatcherMode::Auto).unwrap();
        // A pattern anchored by `^` in every alternative tries index 0
        // only (TASK-100.30). `(?:)` in front hides the anchor from that
        // shortcut and changes nothing else, so the plain search over the
        // same matcher is the reference for it.
        let oracle = back.starts_anchored().then(|| {
            let mut q = u("(?:)");
            q.extend_from_slice(&p);
            let o = Regex::compile_with(&q, &flags, MatcherMode::BacktrackingOnly).unwrap();
            assert!(!o.starts_anchored(), "/{}/{flags_s}", pattern.escape_debug());
            o
        });
        let is_eligible = lin.linear_eligibility().is_ok();
        eligible += is_eligible as usize;
        anchored += oracle.is_some() as usize;
        for _ in 0..INPUTS_PER_PATTERN {
            let input = random_input(&mut rng);
            let a = outcomes(&back, &input, &flags);
            comparisons += a.len();
            if let Some(o) = &oracle {
                let c = outcomes(o, &input, &flags);
                compare(("anchored shortcut", &a), ("plain search", &c), &pattern, &flags_s, &input);
            }
            if is_eligible {
                let b = outcomes(&lin, &input, &flags);
                compare(("backtracking", &a), ("linear", &b), &pattern, &flags_s, &input);
            }
            let c = outcomes(&auto, &input, &flags);
            compare(("backtracking", &a), ("auto", &c), &pattern, &flags_s, &input);
        }
    }
    println!(
        "{valid} patterns ({comparisons} compared outcomes), {eligible} eligible, {invalid} invalid, {anchored} anchored"
    );
    assert!(eligible < valid, "the generator should also produce ineligible patterns");
    assert!(anchored > 0, "the generator should also produce anchored patterns");
}

/// Patterns where regress is known to be wrong, and the ticket that
/// records each divergence. The backtracking matcher follows the
/// specification there; tests/regress_bugs.rs pins the right answers.
fn regress_bug(pattern: &str, flags: &Flags) -> Option<&'static str> {
    let unicode = flags.unicode_mode();
    let icase = may_fold(pattern, flags);
    let lower = pattern.to_ascii_lowercase();
    if !unicode && (lower.contains("\\ud8") || lower.contains("\\ud9") || lower.contains("\\uda") || lower.contains("\\udb")) {
        // Escaped lead surrogates outside u mode join a following escape.
        return Some("TASK-105");
    }
    if !unicode && pattern.contains("\\u{") {
        return Some("TASK-106");
    }
    if unicode && icase && (pattern.contains("\\b") || pattern.contains("\\B")) {
        return Some("TASK-107");
    }
    if !unicode && icase && (pattern.contains(['\u{17F}', '\u{212A}']) || pattern.contains("\\w") || pattern.contains("\\W")) {
        // Found by this test (TASK-100.31): outside u mode, regress folds
        // U+017F LATIN SMALL LETTER LONG S with s and U+212A KELVIN SIGN
        // with k, as u mode does. Canonicalize keeps both as themselves
        // there (their uppercase forms are ASCII or themselves), so /s/i
        // must not match U+017F and /\w/i must not match U+212A. The test
        // also leaves the two out of the inputs of such patterns.
        return Some("U+017F and U+212A under i outside u mode");
    }
    if unicode && icase && pattern.contains("\\W") {
        // Found by this test (TASK-100.31): inside a class under u and i,
        // regress's \W keeps the characters that fold to word characters,
        // so /[\W]/ui matches "s".
        return Some("\\W under ui");
    }
    if flags.unicode_sets && icase {
        // Found by this test (TASK-100.31): under v and i, regress does
        // not fold U+212A KELVIN SIGN with k (/^K/vi misses it, /^K/ui
        // does not), and it applies set operations before case folding,
        // where v mode folds each operand first (MaybeSimpleCaseFolding),
        // so /[\p{L}--[a-z]]/vi matches "c".
        return Some("v mode under i");
    }
    if flags.unicode_sets && pattern.contains("[\\b") {
        // Found by this test (TASK-100.31): in a v-mode class regress reads
        // \b as b, not U+0008 BACKSPACE, so /[\b]/v matches "b".
        return Some("[\\b] in v mode");
    }
    if flags.unicode_sets && pattern.contains("|}") {
        // Found by this test (TASK-100.31): regress never matches the
        // empty string of a class like [\q{abc|a|}b].
        return Some("empty \\q{} string");
    }
    if refers_inside_its_own_group(pattern) {
        // Found by this test (TASK-100.31): a backreference inside the
        // group it names must match empty (the group has not closed), but
        // after a greedy loop before it backs off regress compares against
        // a stale capture, so /(.*\1)./ misses "a".
        return Some("backreference inside its own group");
    }
    None
}

/// Whether a `\N` or `\k<name>` occurs inside group N or a group with that
/// name, after a quantifier inside that group. Escapes and classes are
/// skipped; names are compared as written.
fn refers_inside_its_own_group(pattern: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    // Open parentheses: the group number and name of capturing ones, and
    // whether a quantifier has been seen inside.
    let mut open: Vec<(Option<(usize, String)>, bool)> = Vec::new();
    let quantified = |open: &[(Option<(usize, String)>, bool)], hit: &dyn Fn(&(usize, String)) -> bool| {
        open.iter().any(|(g, q)| *q && g.as_ref().is_some_and(hit))
    };
    let (mut groups, mut i, mut class) = (0usize, 0usize, 0usize);
    while i < p.len() {
        match p[i] {
            '\\' if i + 1 < p.len() => {
                let next = p[i + 1];
                if class == 0 && next.is_ascii_digit() && next != '0' {
                    let digits: String = p[i + 1..].iter().take_while(|c| c.is_ascii_digit()).collect();
                    let n: usize = digits.parse().unwrap_or(0);
                    if quantified(&open, &|(g, _)| *g == n) {
                        return true;
                    }
                } else if class == 0 && next == 'k' && p.get(i + 2) == Some(&'<') {
                    let name: String = p[i + 3..].iter().take_while(|&&c| c != '>').collect();
                    if quantified(&open, &|(_, m)| *m == name) {
                        return true;
                    }
                }
                i += 2;
                continue;
            }
            '[' => class += 1,
            ']' if class > 0 => class -= 1,
            '(' if class == 0 => {
                let named = p.get(i + 1) == Some(&'?') && p.get(i + 2) == Some(&'<') && !matches!(p.get(i + 3), Some('=' | '!'));
                if p.get(i + 1) != Some(&'?') || named {
                    groups += 1;
                    let name = if named { p[i + 3..].iter().take_while(|&&c| c != '>').collect() } else { String::new() };
                    open.push((Some((groups, name)), false));
                } else {
                    open.push((None, false));
                }
                if p.get(i + 1) == Some(&'?') {
                    // Not a quantifier.
                    i += 2;
                    continue;
                }
            }
            ')' if class == 0 => {
                open.pop();
            }
            '*' | '+' | '?' | '{' if class == 0 => open.iter_mut().for_each(|(_, q)| *q = true),
            _ => {}
        }
        i += 1;
    }
    false
}

#[test]
fn self_references_are_recognised() {
    assert!(refers_inside_its_own_group(r"(.*\1)."));
    assert!(refers_inside_its_own_group(r"(a*(?:b\1))"));
    assert!(refers_inside_its_own_group(r"(?<g1>x+\k<g1>)"));
    assert!(refers_inside_its_own_group(r"()(a?\2)"));
    assert!(refers_inside_its_own_group(r"((?:ab){2}\1)"));
    assert!(!refers_inside_its_own_group(r"(a\1)"));
    assert!(!refers_inside_its_own_group(r"(?:a*)(?<g1>\k<g1>)"));
    assert!(!refers_inside_its_own_group(r"(a)\1"));
    assert!(!refers_inside_its_own_group(r"\1(a)"));
    assert!(!refers_inside_its_own_group(r"(?<g1>x)\k<g1>"));
    assert!(!refers_inside_its_own_group(r"(?:a\1)(b)"));
    assert!(!refers_inside_its_own_group(r"[(]\1(a)"));
    assert!(!refers_inside_its_own_group(r"(a[\1])"));
    assert!(!refers_inside_its_own_group(r"(?<=a\1)(b)"));
}

/// Whether any part of the pattern matches ignoring case (the i flag or
/// an i modifier).
fn may_fold(pattern: &str, flags: &Flags) -> bool {
    flags.ignore_case || pattern.contains("(?i")
}

#[test]
fn backtracking_agrees_with_regress() {
    let (cases, mut rng, trace) = settings();
    let watchdog = Watchdog::start();
    let (mut compared, mut invalid, mut rejected, mut comparisons) = (0usize, 0usize, 0usize, 0usize);
    let mut known = std::collections::BTreeMap::<&str, usize>::new();
    while compared < cases {
        let (pattern, flags_s) = random_case(&mut rng, 2, true);
        let p = u(&pattern);
        let flags = Flags::parse(&u(&flags_s)).unwrap();
        watchdog.case(&pattern, &flags_s);
        if trace {
            eprintln!("/{}/{flags_s}", pattern.escape_debug());
        }
        let Ok(back) = Regex::compile_with(&p, &flags, MatcherMode::BacktrackingOnly) else {
            invalid += 1;
            continue;
        };
        if let Some(bug) = regress_bug(&pattern, &flags) {
            *known.entry(bug).or_default() += 1;
            continue;
        }
        let Some(reference) = Reference::compile(&p, &flags) else {
            // Valid syntax regress does not support.
            rejected += 1;
            continue;
        };
        compared += 1;
        for _ in 0..INPUTS_PER_PATTERN {
            let mut input = random_input(&mut rng);
            if !flags.unicode_mode() && may_fold(&pattern, &flags) {
                // See the U+017F and U+212A case of `regress_bug`.
                input.retain(|&c| c != 0x17F && c != 0x212A);
            }
            let a = outcomes(&back, &input, &flags);
            let b = outcomes(&reference, &input, &flags);
            comparisons += a.len();
            compare(("backtracking", &a), ("regress", &b), &pattern, &flags_s, &input);
        }
    }
    println!(
        "{compared} patterns ({comparisons} compared outcomes), {invalid} invalid, {rejected} rejected by regress, {} skipped as known regress bugs: {known:?}",
        known.values().sum::<usize>()
    );
}

/// Every compiled program loads back from its bytes and matches the same
/// (ADR 0010 addendum), and corrupted bytes either fail to load or load
/// into a program that runs without panicking. Corrupted programs run
/// only where a step budget bounds them (Auto with an eligible linear
/// program, or the linear matcher).
#[test]
fn programs_survive_serialization_and_corruption() {
    let (cases, mut rng, trace) = settings();
    let watchdog = Watchdog::start();
    let (mut valid, mut corrupted, mut loaded, mut unbudgeted) = (0usize, 0usize, 0usize, 0usize);
    while valid < cases {
        let (pattern, flags_s) = random_case(&mut rng, 3, false);
        let p = u(&pattern);
        let flags = Flags::parse(&u(&flags_s)).unwrap();
        watchdog.case(&pattern, &flags_s);
        if trace {
            eprintln!("/{}/{flags_s}", pattern.escape_debug());
        }
        let Ok(re) = Regex::compile_with(&p, &flags, MatcherMode::Auto) else { continue };
        valid += 1;
        let bytes = re.program_bytes();
        let input = random_input(&mut rng);
        for mode in [MatcherMode::Auto, MatcherMode::BacktrackingOnly, MatcherMode::LinearWhenEligible] {
            let again = Regex::from_program_bytes(&bytes, mode)
                .unwrap_or_else(|e| panic!("/{}/{flags_s} does not load: {e}", pattern.escape_debug()));
            assert_eq!(again.program_bytes(), bytes, "/{}/{flags_s}", pattern.escape_debug());
            let original = Regex::compile_with(&p, &flags, mode).unwrap();
            compare(("compiled", &outcomes(&original, &input, &flags)), ("loaded", &outcomes(&again, &input, &flags)), &pattern, &flags_s, &input);
        }
        for _ in 0..4 {
            let mut bad = bytes.clone();
            for _ in 0..1 + rng.below(3) {
                let k = rng.below(bad.len());
                bad[k] = match rng.below(4) {
                    0 => bad[k].wrapping_add(1),
                    1 => bad[k].wrapping_sub(1),
                    2 => bad[k] ^ (1 << rng.below(8)),
                    _ => rng.below(256) as u8,
                };
            }
            corrupted += 1;
            for mode in [MatcherMode::Auto, MatcherMode::LinearWhenEligible] {
                if let Ok(r) = Regex::from_program_bytes(&bad, mode) {
                    loaded += 1;
                    if r.linear_eligibility().is_err() {
                        // It would backtrack without a step budget, and a
                        // corrupted loop count can make that exponential
                        // as in any compiled pattern (`(?:()?|\B){91,}?`).
                        unbudgeted += 1;
                        continue;
                    }
                    if trace {
                        eprintln!("  corrupted {mode:?} on \"{}\": {bad:?}", show(&input));
                    }
                    let _ = outcomes(&r, &input, &flags);
                }
            }
        }
    }
    println!(
        "{valid} programs round trip; {corrupted} corrupted copies, {loaded} loads of them, {} run, {unbudgeted} not run (no step budget)",
        loaded - unbudgeted
    );
}

/// Wrappers for one level of nesting around a fuzzed pattern: they keep
/// backtracking polynomial however deep they go (no unbounded quantifier,
/// which nested would be exponential on a failing input).
const NESTING: &[(&str, &str)] = &[
    ("(", ")"),
    ("(?:", ")"),
    ("(?:", ")?"),
    ("(", ")??"),
    ("(?:b|", ")"),
    ("(?:", "|c)"),
    ("(?i:", ")"),
    ("(?-s:", ")"),
    ("(?=", ")"),
    ("(?!", ")"),
    ("(?<=", ")"),
    ("(?<!", ")"),
];

/// A random pattern nested in random wrappers, some of them deep, some
/// past [`syntax::MAX_NESTING`]; in `v` mode the pattern sometimes begins
/// with a nested class.
fn deep_case(rng: &mut Rng) -> (String, String, usize) {
    let (inner, flags) = random_case(rng, 2, false);
    let limit = agnijs_js_regex::syntax::MAX_NESTING;
    let mut depth = match rng.below(10) {
        0..=5 => 1 + rng.below(40),
        6..=8 => limit - 8 + rng.below(12),
        _ => limit + rng.below(limit * 4),
    };
    let mut pattern = inner;
    if flags.ends_with('v') && rng.chance(40) {
        let k = 1 + rng.below(depth);
        let item = rng.pick(&["a", "[ab]", "\\q{ab|c}", "[^[ab]]", "[\\w--[a]]", "\\d"]);
        pattern = format!("{}{item}{}{pattern}", "[".repeat(k), "]".repeat(k));
        depth -= k.min(depth - 1);
    }
    // Without lookaround half the time, so that many stay eligible for the
    // linear matcher.
    let wrappers = if rng.chance(50) { NESTING } else { &NESTING[..8] };
    for _ in 0..depth {
        let (open, close) = wrappers[rng.below(wrappers.len())];
        pattern = format!("{open}{pattern}{close}");
    }
    (pattern, flags, depth)
}

/// The matchers agree on deeply nested patterns as on shallow ones, and a
/// pattern nested past the limit is a syntax error from every entry point
/// (TASK-100.34). Runs on an explicit 2 MB stack, the default for a spawned
/// thread.
#[test]
fn matchers_agree_on_deeply_nested_patterns() {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(deeply_nested_patterns)
        .unwrap()
        .join()
        .unwrap();
}

fn deeply_nested_patterns() {
    let (cases, mut rng, trace) = settings();
    let cases = cases.div_ceil(10);
    let watchdog = Watchdog::start();
    let (mut valid, mut eligible, mut too_deep, mut invalid, mut deepest) = (0usize, 0usize, 0usize, 0usize, 0usize);
    while valid < cases {
        let (pattern, flags_s, _) = deep_case(&mut rng);
        let p = u(&pattern);
        let flags = Flags::parse(&u(&flags_s)).unwrap();
        watchdog.case(&pattern, &flags_s);
        if trace {
            eprintln!("/{}/{flags_s}", pattern.escape_debug());
        }
        let back = match Regex::compile_with(&p, &flags, MatcherMode::BacktrackingOnly) {
            Ok(back) => back,
            Err(e) => {
                if e.contains("nested too deeply") {
                    too_deep += 1;
                    assert!(agnijs_js_regex::syntax::validate_units(&p, flags.unicode, flags.unicode_sets).is_err());
                    for mode in [MatcherMode::LinearWhenEligible, MatcherMode::Auto] {
                        assert!(Regex::compile_with(&p, &flags, mode).is_err(), "/{}/{flags_s}", pattern.escape_debug());
                    }
                } else {
                    invalid += 1;
                }
                continue;
            }
        };
        valid += 1;
        deepest = deepest.max(pattern.matches('(').count());
        let lin = Regex::compile_with(&p, &flags, MatcherMode::LinearWhenEligible).unwrap();
        let auto = Regex::compile_with(&p, &flags, MatcherMode::Auto).unwrap();
        let loaded = Regex::from_program_bytes(&back.program_bytes(), MatcherMode::BacktrackingOnly).unwrap();
        let is_eligible = lin.linear_eligibility().is_ok();
        eligible += is_eligible as usize;
        for _ in 0..INPUTS_PER_PATTERN / 2 {
            let input = random_input(&mut rng);
            let a = outcomes(&back, &input, &flags);
            if is_eligible {
                compare(("backtracking", &a), ("linear", &outcomes(&lin, &input, &flags)), &pattern, &flags_s, &input);
            }
            compare(("backtracking", &a), ("auto", &outcomes(&auto, &input, &flags)), &pattern, &flags_s, &input);
            compare(("compiled", &a), ("loaded", &outcomes(&loaded, &input, &flags)), &pattern, &flags_s, &input);
        }
    }
    println!(
        "{valid} deeply nested patterns (up to {deepest} open parentheses), {eligible} eligible, {too_deep} too deep, {invalid} otherwise invalid"
    );
    assert!(too_deep > 0, "the generator should also nest past the limit");
    assert!(eligible > 0, "the generator should also produce eligible patterns");
}

/// Fail with the reproduction at the first differing outcome.
fn compare(
    (name_a, a): (&str, &[Option<Captures>]),
    (name_b, b): (&str, &[Option<Captures>]),
    pattern: &str,
    flags: &str,
    input: &[u16],
) {
    if a != b {
        let k = a.iter().zip(b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
        panic!(
            "mismatch for /{}/{flags} on \"{}\" at outcome {k}:\n  {name_a}: {:?}\n  {name_b}: {:?}",
            pattern.escape_debug(),
            show(input),
            a.get(k),
            b.get(k),
        );
    }
}
