//! Seeded mutation for the bounded runner. Not coverage-guided: libFuzzer
//! does that through `fuzz/`. Every choice comes from [`Rng`], so a seed
//! reproduces a run on every platform.

/// xorshift64*.
pub struct Rng(pub u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.max(1))
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }
    pub fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// Tokens spliced into text inputs: JavaScript syntax that reaches the
/// parser's and compiler's less travelled paths, regex syntax, JSON and
/// protocol fragments.
const TOKENS: &[&str] = &[
    "(", ")", "{", "}", "[", "]", ";", ",", ".", "...", "?.", "??", "?", ":", "=>", "=", "+=", "**=", "&&=", "||=",
    "??=", "<<", ">>>", "**", "++", "--", "!", "~", "typeof ", "void ", "delete ", "new ", "new.target", "super",
    "super.x", "this", "yield", "yield*", "await", "async ", "function", "function*", "class", "extends", "static",
    "get ", "set ", "#p", "in", "instanceof", "of", "let", "const", "var", "using", "await using", "if", "else",
    "for", "while", "do", "switch", "case", "default", "break", "continue", "return", "throw", "try", "catch",
    "finally", "with", "debugger", "import", "export", "import.meta", "import(", "from", "as", "label:", "`",
    "`${", "}`", "'", "\"", "\\", "\\u{", "\\u0041", "\\x41", "/", "/a+/g", "/(?<n>a)\\k<n>/v", "0x", "0b1", "0o7",
    "1e309", "1_0", "1n", "0.", ".5", "NaN", "-0", "2**53", "null", "undefined", "true", "false", "arguments",
    "eval", "\"use strict\";", "<!--", "-->", "/*", "*/", "//", "\n", "\u{2028}", "\u{feff}", "\u{1F600}",
    "\u{D7FF}", "Symbol.iterator", "Proxy", "Reflect", "Promise", "BigInt", "WeakRef", "Atomics", "Intl",
    "JSON", "RegExp", "Array(1e6)", ".length", ".constructor", ".__proto__", "Object.prototype", "[[]]", "{}",
    "(?=", "(?!", "(?<=", "(?<!", "(?:", "(?i:", "\\p{L}", "\\P{Script=Greek}", "[^", "[\\d--\\w]", "[[a]&&[b]]",
    "{1,}", "{2,3}", "*?", "+?", "\\b", "\\B", "^", "$", "|", "\\1", "\\k<n>", "\\c", "\\0", "[\\s\\S]", "\"a\":",
    "[1,", "1e-400", "\"\\ud800\"", "-", "\"id\":", "\"method\":", "\"params\":", "\"Runtime.evaluate\"",
    "\"Debugger.paused\"", "\"objectId\":\"1\"", "\"callFrameId\":\"0\"", "\"expression\":", "\"mappings\":",
    "\"sources\":", "\"imports\":", "\"scopes\":", "en-US", "de-u-co-phonebk", "ar-EG-u-nu-arab", "x-private",
    "und", "Etc/GMT+14", "America/New_York", "EUR", "unit", "percent", "kilometer-per-hour", "compact", "full",
    "gregory", "islamic-umalqura", "latn", "grapheme", "word", "sentence", "-u-", "-t-",
];

/// Edit text: delete a run, splice a token, duplicate a piece, replace a
/// character, splice from another input, or swap two lines.
pub fn text(rng: &mut Rng, input: &[u8], others: &[Vec<u8>]) -> Vec<u8> {
    let mut s: Vec<char> = String::from_utf8_lossy(input).chars().collect();
    for _ in 0..1 + rng.below(2) {
        let len = s.len();
        match rng.below(16) {
            0 | 1 if len > 0 => {
                let at = rng.below(len);
                let n = 1 + rng.below(8.min(len - at));
                s.drain(at..at + n);
            }
            2..=5 | 15 => {
                let at = rng.below(len + 1);
                let t: Vec<char> = rng.pick(TOKENS).chars().collect();
                s.splice(at..at, t);
            }
            6 | 7 if len > 1 => {
                let at = rng.below(len);
                let n = 1 + rng.below((len - at).min(32));
                let piece: Vec<char> = s[at..at + n].to_vec();
                let to = rng.below(len + 1);
                // Repeat it sometimes, to build nesting.
                for _ in 0..if rng.chance(8) { 1 + rng.below(64) } else { 1 } {
                    s.splice(to..to, piece.iter().copied());
                }
            }
            8..=10 if len > 0 => {
                let at = rng.below(len);
                s[at] = match rng.below(4) {
                    0 => char::from_u32(rng.below(0x80) as u32).unwrap_or(' '),
                    1 => char::from_u32(rng.below(0x3000) as u32).unwrap_or('\u{FFFD}'),
                    _ => char::from_u32(32 + rng.below(95) as u32).unwrap_or(' '),
                };
            }
            11 | 12 if !others.is_empty() => {
                let other: Vec<char> = String::from_utf8_lossy(rng.pick(others)).chars().collect();
                if !other.is_empty() {
                    let from = rng.below(other.len());
                    let n = 1 + rng.below((other.len() - from).min(64));
                    let at = rng.below(len + 1);
                    s.splice(at..at, other[from..from + n].iter().copied());
                }
            }
            13 => {
                let mut lines: Vec<String> = s.iter().collect::<String>().split('\n').map(str::to_string).collect();
                if lines.len() > 1 {
                    let a = rng.below(lines.len());
                    let b = rng.below(lines.len());
                    lines.swap(a, b);
                    s = lines.join("\n").chars().collect();
                }
            }
            14 if len > 0 => {
                // Truncate: unterminated constructs.
                s.truncate(rng.below(len));
            }
            _ => {}
        }
    }
    s.into_iter().collect::<String>().into_bytes()
}

const INTERESTING: &[u64] = &[0, 1, 2, 0x7f, 0x80, 0xff, 0x100, 0x7fff, 0xffff, 0x7fff_ffff, 0xffff_ffff, u64::MAX];

/// Edit binary data: flip a bit, set a byte, write an interesting 1, 2, 4
/// or 8 byte value, insert or delete bytes, copy a range, or splice from
/// another input.
pub fn binary(rng: &mut Rng, input: &[u8], others: &[Vec<u8>]) -> Vec<u8> {
    let mut b = input.to_vec();
    for _ in 0..1 + rng.below(2) {
        let len = b.len();
        match rng.below(16) {
            0..=3 if len > 0 => {
                let at = rng.below(len);
                b[at] ^= 1 << rng.below(8);
            }
            4..=6 if len > 0 => {
                let at = rng.below(len);
                b[at] = rng.next_u64() as u8;
            }
            7..=10 if len > 0 => {
                let width = *rng.pick(&[1usize, 2, 4, 8]);
                let at = rng.below(len);
                let mut v = *rng.pick(INTERESTING);
                if rng.chance(3) {
                    v = v.wrapping_add(rng.below(3) as u64).wrapping_sub(1);
                }
                for (i, byte) in v.to_le_bytes().iter().take(width).enumerate() {
                    if let Some(slot) = b.get_mut(at + i) {
                        *slot = *byte;
                    }
                }
            }
            11 => {
                let at = rng.below(len + 1);
                let n = 1 + rng.below(16);
                let fill: Vec<u8> = (0..n).map(|_| rng.next_u64() as u8).collect();
                b.splice(at..at, fill);
            }
            12 if len > 0 => {
                let at = rng.below(len);
                let n = 1 + rng.below((len - at).min(16));
                b.drain(at..at + n);
            }
            13 if len > 1 => {
                let from = rng.below(len);
                let n = 1 + rng.below((len - from).min(64));
                let piece = b[from..from + n].to_vec();
                let to = rng.below(len + 1);
                b.splice(to..to, piece);
            }
            14 if !others.is_empty() => {
                let other = rng.pick(others);
                if !other.is_empty() {
                    let from = rng.below(other.len());
                    let n = 1 + rng.below((other.len() - from).min(64));
                    let at = rng.below(len + 1);
                    let end = (at + n).min(len);
                    b.splice(at..end, other[from..from + n].iter().copied());
                }
            }
            15 if len > 0 => {
                b.truncate(rng.below(len));
            }
            _ => {}
        }
    }
    b
}
