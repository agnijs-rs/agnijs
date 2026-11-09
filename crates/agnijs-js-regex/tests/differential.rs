//! Differential fuzzing: the linear-time matcher against the backtracking
//! matcher (ADR 0013 addendum).
//!
//! Random patterns from an ECMAScript grammar subset (captures, named and
//! duplicate-named groups, modifiers, every quantifier form, assertions,
//! classes, escapes, Annex B forms, `u` and `v` mode syntax) run over random
//! short inputs under random flags. Each eligible pattern is matched with
//! both matchers from every start index, sticky from every index, and
//! through a global `lastIndex` loop that advances past empty matches the
//! way `RegExpBuiltinExec` and `AdvanceStringIndex` do. Any difference in
//! the match or in any capture fails the test with the reproduction.
//!
//! `cargo test` (and so CI) runs [`MIN_CASES`] eligible patterns, each on
//! [`INPUTS_PER_PATTERN`] inputs, with a fixed seed. `AGNIJS_REGEX_FUZZ_CASES`
//! changes the count, `AGNIJS_REGEX_FUZZ_SEED` picks another seed and
//! `AGNIJS_REGEX_FUZZ_TRACE` prints each pattern before it runs. About 15 s
//! in a debug build. Keep heavier runs bounded (one process, a raised
//! count, a timeout) rather than looping locally. A watchdog aborts the
//! run, naming the pattern, when any single case hangs.
//!
//! The generator nests quantifiers at most two deep, and a quantified atom
//! inside a quantified group is a single character. Deeper nesting reaches
//! patterns the backtracking matcher itself never finishes (for example
//! `(?:(_|)+){2}K` on `"__"`), where there is no reference result to
//! compare against.

// Both matchers are needed; without the `linear` feature there is only one.
#![cfg(feature = "linear")]

use agnijs_js_regex::{Captures, Flags, MatcherMode, Regex};

/// Eligible patterns compared per run in CI, each against several inputs.
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
    "\\p{RGI_Emoji}", "[a&&a]",
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
        // At most two quantifiers nest: deeper nesting makes the
        // backtracking reference itself exponential on short inputs
        // (`((?:b*)*){2}` inside `{1,2}` runs for minutes on 8 b's), which
        // is the problem being solved, not something to compare against.
        let quantify = self.quantifiers < 2 && self.rng.chance(40);
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
        // A second-level quantifier applies to a single-character atom
        // only: a quantified group inside a quantified group can hang the
        // backtracking reference outright (`(?:(_|)+){2}K` on "__" never
        // returns).
        let r = if self.quantifiers >= 2 { self.rng.below(60) } else { self.rng.below(100) };
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
            0 | 1 | 2 => format!("({body})"),
            3 | 4 => format!("(?:{body})"),
            5 => {
                self.names += 1;
                // Reuse a name sometimes: duplicates are legal only in
                // different alternatives, so some of these fail to compile
                // and are skipped.
                let n = if self.names > 1 && self.rng.chance(30) { self.names - 1 } else { self.names };
                format!("(?<g{n}>{body})")
            }
            6 => format!("(?{}:{body})", self.rng.pick(&["i", "-i", "m", "s", "-s", "i-m", "-ms"])),
            _ => {
                // Ineligible constructs stay in the mix to exercise routing.
                let k = self.rng.below(6);
                match k {
                    0 => format!("(?={body})"),
                    1 => format!("(?<!{body})"),
                    2 => format!("({body})\\1"),
                    _ => format!("({body})"),
                }
            }
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

/// Every observable outcome of matching `re` against `input`: a search
/// from every index, a sticky attempt at every index, and the global or
/// sticky `lastIndex` loop.
fn outcomes(re: &Regex, input: &[u16], flags: &Flags) -> Vec<Option<Captures>> {
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
        let mut attempt = last;
        if unicode
            && attempt > 0
            && attempt < input.len()
            && (0xDC00..0xE000).contains(&input[attempt])
            && (0xD800..0xDC00).contains(&input[attempt - 1])
        {
            attempt -= 1;
        }
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
/// [`Watchdog::LIMIT`]: a hang in either matcher must fail fast rather
/// than hold a CPU.
struct Watchdog {
    state: std::sync::Arc<std::sync::Mutex<(u64, String)>>,
}

impl Watchdog {
    const LIMIT: std::time::Duration = std::time::Duration::from_secs(20);

    fn start() -> Watchdog {
        let state = std::sync::Arc::new(std::sync::Mutex::new((0u64, String::new())));
        let s = state.clone();
        std::thread::spawn(move || {
            let mut last = (u64::MAX, std::time::Instant::now());
            loop {
                std::thread::sleep(std::time::Duration::from_millis(500));
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

#[test]
fn linear_matcher_agrees_with_backtracking() {
    let cases: usize = std::env::var("AGNIJS_REGEX_FUZZ_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(MIN_CASES);
    let seed: u64 = std::env::var("AGNIJS_REGEX_FUZZ_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x5EED_0013);
    let trace = std::env::var_os("AGNIJS_REGEX_FUZZ_TRACE").is_some();
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let watchdog = Watchdog::start();
    let (mut eligible, mut ineligible, mut invalid, mut comparisons) = (0usize, 0usize, 0usize, 0usize);
    while eligible < cases {
        let mode = match rng.below(10) {
            0..=5 => Mode::Legacy,
            6..=7 => Mode::U,
            _ => Mode::V,
        };
        let pattern = Gen { rng: &mut rng, mode, names: 0, quantifiers: 0 }.disjunction(0);
        let flags_s = random_flags(&mut rng, mode);
        let p: Vec<u16> = pattern.encode_utf16().collect();
        let f16: Vec<u16> = flags_s.encode_utf16().collect();
        let flags = Flags::parse(&f16).unwrap();
        watchdog.case(&pattern, &flags_s);
        if trace {
            eprintln!("/{}/{flags_s}", pattern.escape_debug());
        }
        let Ok(back) = Regex::compile_with(&p, &flags, MatcherMode::BacktrackingOnly) else {
            invalid += 1;
            continue;
        };
        let lin = Regex::compile_with(&p, &flags, MatcherMode::LinearWhenEligible).unwrap();
        if lin.linear_eligibility().is_err() {
            ineligible += 1;
            continue;
        }
        eligible += 1;
        for _ in 0..INPUTS_PER_PATTERN {
            let input = random_input(&mut rng);
            let a = outcomes(&back, &input, &flags);
            let b = outcomes(&lin, &input, &flags);
            comparisons += a.len();
            if a != b {
                let k = a.iter().zip(&b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
                panic!(
                    "mismatch for /{}/{} on \"{}\" at outcome {k}:\n  backtracking: {:?}\n  linear:       {:?}",
                    pattern.escape_debug(),
                    flags_s,
                    show(&input),
                    a.get(k),
                    b.get(k),
                );
            }
        }
    }
    println!(
        "{eligible} eligible patterns ({comparisons} compared outcomes), {ineligible} ineligible, {invalid} invalid"
    );
    assert!(ineligible > 0, "the generator should also produce ineligible patterns");
}
