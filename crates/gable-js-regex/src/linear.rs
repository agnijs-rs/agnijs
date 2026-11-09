//! The linear-time matcher (ADR 0013 addendum).
//!
//! A Pike VM over UTF-16 input with ECMAScript leftmost-first priority,
//! for patterns without backreferences or lookaround. Every thread runs
//! in lock step over the input and two threads at the same program state
//! and position are merged (the higher-priority one survives), so a match
//! costs O(program states x input length) whatever the pattern.
//!
//! ECMAScript semantics carried over from the backtracking definition:
//!
//! - **Priority.** Threads are kept in the order backtracking would try
//!   them, so the surviving match is the one backtracking finds.
//! - **Capture reset.** Every quantifier iteration starts with the captures
//!   inside the quantified atom unset (RepeatMatcher step 4).
//! - **Empty iterations.** An iteration past the minimum that consumes
//!   nothing fails (RepeatMatcher's continuation). Each thread carries one
//!   "consumed since this iteration began" bit per nesting level of
//!   quantifiers whose body can match empty; the bits are part of the
//!   thread's state for merging, so merging stays sound.
//! - **Counted quantifiers** are unrolled (`a{2,4}` is `aa(?:a(?:a)?)?`).
//!
//! What a single character matches is not decided here: each character
//! class, escape and case-insensitive literal becomes a one-character
//! predicate that `regress` itself evaluates (memoized per character), so
//! case folding, `\w` under `ui`, property escapes and `v`-mode set
//! operations agree with the backtracking matcher by construction. Classes
//! that can match strings (`\q{..}`, properties of strings) are not
//! eligible.
//!
//! Routing is conservative: anything the parser below does not recognise
//! makes the pattern ineligible and it stays on the backtracking matcher.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

/// Largest program, in instructions, after unrolling counted quantifiers.
const MAX_PROGRAM: usize = 20_000;
/// Most nesting levels of quantifiers with a nullable body: each level
/// doubles the number of thread states.
const MAX_EMPTY_DEPTH: u32 = 4;
/// Largest thread table (states x capture slots) the matcher allocates.
const MAX_TABLE: usize = 1 << 18;

const NONE: usize = usize::MAX;

/// Why a pattern is not eligible for the linear matcher.
pub(crate) type Ineligible = &'static str;

#[derive(Clone, Copy, Debug)]
struct Mods {
    i: bool,
    m: bool,
    s: bool,
}

#[derive(Debug)]
enum Node {
    Empty,
    Lit(u32),
    Pred(u32),
    Dot { all: bool },
    Start { multiline: bool },
    End { multiline: bool },
    Word { invert: bool, pred: u32 },
    Group(Option<usize>, Box<Node>),
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat {
        body: Box<Node>,
        min: u32,
        max: Option<u32>,
        greedy: bool,
        /// Capture groups inside the body: `first..first + count`.
        first: usize,
        count: usize,
    },
}

fn nullable(n: &Node) -> bool {
    match n {
        Node::Empty | Node::Start { .. } | Node::End { .. } | Node::Word { .. } => true,
        Node::Lit(_) | Node::Pred(_) | Node::Dot { .. } => false,
        Node::Group(_, b) => nullable(b),
        Node::Concat(v) => v.iter().all(nullable),
        Node::Alt(v) => v.iter().any(nullable),
        Node::Repeat { body, min, .. } => *min == 0 || nullable(body),
    }
}

// ---- character predicates -------------------------------------------------------------------

/// One character class, escape or case-insensitive literal, decided by
/// `regress` on a one-character input and memoized.
struct Pred {
    re: regress::Regex,
    unicode: bool,
    /// ASCII answers: 0 unknown, 1 no, 2 yes.
    ascii: [Cell<u8>; 128],
    other: RefCell<HashMap<u32, bool>>,
}

impl Pred {
    fn test(&self, c: u32) -> bool {
        if c < 128 {
            let slot = &self.ascii[c as usize];
            match slot.get() {
                1 => return false,
                2 => return true,
                _ => {}
            }
            let r = self.eval(c);
            slot.set(if r { 2 } else { 1 });
            return r;
        }
        if let Some(&r) = self.other.borrow().get(&c) {
            return r;
        }
        let r = self.eval(c);
        self.other.borrow_mut().insert(c, r);
        r
    }

    fn eval(&self, c: u32) -> bool {
        let mut buf = [0u16; 2];
        let units: &[u16] = if c > 0xFFFF {
            let v = c - 0x10000;
            buf = [0xD800 + (v >> 10) as u16, 0xDC00 + (v & 0x3FF) as u16];
            &buf
        } else {
            buf[0] = c as u16;
            &buf[..1]
        };
        if self.unicode {
            self.re.find_from_utf16(units, 0).next().is_some()
        } else {
            self.re.find_from_ucs2(units, 0).next().is_some()
        }
    }
}

struct PredTable {
    preds: Vec<Pred>,
    index: HashMap<(Vec<u32>, bool), u32>,
    unicode: bool,
    unicode_sets: bool,
}

impl PredTable {
    /// The predicate for the one-character atom `src` under `icase`.
    fn get(&mut self, src: &[u32], icase: bool) -> Result<u32, Ineligible> {
        let key = (src.to_vec(), icase);
        if let Some(&id) = self.index.get(&key) {
            return Ok(id);
        }
        let mut p: Vec<u32> = "^(?:".chars().map(|c| c as u32).collect();
        p.extend_from_slice(src);
        p.extend(")$".chars().map(|c| c as u32));
        let flags = regress::Flags {
            icase,
            multiline: false,
            dot_all: false,
            no_opt: false,
            unicode: !self.unicode_sets && self.unicode,
            unicode_sets: self.unicode_sets,
        };
        let re = regress::Regex::from_unicode(p.into_iter(), flags)
            .map_err(|_| "a character atom did not compile on its own")?;
        let id = self.preds.len() as u32;
        self.preds.push(Pred {
            re,
            unicode: self.unicode || self.unicode_sets,
            ascii: std::array::from_fn(|_| Cell::new(0)),
            other: RefCell::new(HashMap::new()),
        });
        self.index.insert(key, id);
        Ok(id)
    }
}

// ---- parsing ---------------------------------------------------------------------------------

const STRING_PROPS: &[&str] = &[
    "Basic_Emoji",
    "Emoji_Keycap_Sequence",
    "RGI_Emoji_Modifier_Sequence",
    "RGI_Emoji_Flag_Sequence",
    "RGI_Emoji_Tag_Sequence",
    "RGI_Emoji_ZWJ_Sequence",
    "RGI_Emoji",
];

fn c(ch: char) -> u32 {
    ch as u32
}

struct Parser<'a> {
    p: &'a [u32],
    i: usize,
    /// Unicode mode (`u` or `v`).
    u: bool,
    v: bool,
    ngroups: usize,
    groups_seen: usize,
    preds: PredTable,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u32> {
        self.p.get(self.i).copied()
    }
    fn peek_at(&self, n: usize) -> Option<u32> {
        self.p.get(self.i + n).copied()
    }
    fn at(&self, ch: char) -> bool {
        self.peek() == Some(c(ch))
    }
    fn eat(&mut self, ch: char) -> bool {
        if self.at(ch) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn disjunction(&mut self, m: Mods) -> Result<Node, Ineligible> {
        let mut alts = vec![self.alternative(m)?];
        while self.eat('|') {
            alts.push(self.alternative(m)?);
        }
        Ok(if alts.len() == 1 { alts.pop().unwrap() } else { Node::Alt(alts) })
    }

    fn alternative(&mut self, m: Mods) -> Result<Node, Ineligible> {
        let mut items = Vec::new();
        while let Some(ch) = self.peek() {
            if ch == c('|') || ch == c(')') {
                break;
            }
            items.push(self.term(m)?);
        }
        Ok(match items.len() {
            0 => Node::Empty,
            1 => items.pop().unwrap(),
            _ => Node::Concat(items),
        })
    }

    fn term(&mut self, m: Mods) -> Result<Node, Ineligible> {
        let ch = self.peek().unwrap();
        if ch == c('^') {
            self.i += 1;
            return Ok(Node::Start { multiline: m.m });
        }
        if ch == c('$') {
            self.i += 1;
            return Ok(Node::End { multiline: m.m });
        }
        if ch == c('\\') && matches!(self.peek_at(1), Some(x) if x == c('b') || x == c('B')) {
            let invert = self.peek_at(1) == Some(c('B'));
            self.i += 2;
            // The backtracking matcher adds the case-folded word
            // characters (U+017F, U+212A) under `ui` only, not `vi`.
            let w = [c('\\'), c('w')];
            let pred = self.preds.get(&w, m.i && self.u && !self.v)?;
            return Ok(Node::Word { invert, pred });
        }
        if ch == c('(') && self.peek_at(1) == Some(c('?')) {
            let a = self.peek_at(2);
            if a == Some(c('=')) || a == Some(c('!')) || a == Some(c('<')) {
                return Err("lookaround");
            }
        }
        let groups_before = self.groups_seen;
        let atom = self.atom(m)?;
        self.quantifier(atom, groups_before)
    }

    /// A braced quantifier `{n}`, `{n,}`, `{n,m}` at the cursor: its
    /// length and bounds (saturated to `u32::MAX`).
    fn braced(&self) -> Option<(usize, u32, Option<u32>)> {
        let mut j = self.i + 1;
        let digits = |j: &mut usize| -> Option<u32> {
            let start = *j;
            let mut v: u64 = 0;
            while let Some(&d) = self.p.get(*j) {
                if (c('0')..=c('9')).contains(&d) {
                    v = (v * 10 + (d - c('0')) as u64).min(u32::MAX as u64);
                    *j += 1;
                } else {
                    break;
                }
            }
            (*j != start).then_some(v as u32)
        };
        let min = digits(&mut j)?;
        let mut max = Some(min);
        if self.p.get(j) == Some(&c(',')) {
            j += 1;
            max = digits(&mut j);
        }
        if self.p.get(j) != Some(&c('}')) {
            return None;
        }
        Some((j + 1 - self.i, min, max))
    }

    fn quantifier(&mut self, atom: Node, groups_before: usize) -> Result<Node, Ineligible> {
        let (min, max) = match self.peek() {
            Some(x) if x == c('*') => {
                self.i += 1;
                (0, None)
            }
            Some(x) if x == c('+') => {
                self.i += 1;
                (1, None)
            }
            Some(x) if x == c('?') => {
                self.i += 1;
                (0, Some(1))
            }
            Some(x) if x == c('{') => match self.braced() {
                Some((len, min, max)) => {
                    self.i += len;
                    (min, max)
                }
                None => return Ok(atom),
            },
            _ => return Ok(atom),
        };
        let greedy = !self.eat('?');
        Ok(Node::Repeat {
            body: Box::new(atom),
            min,
            max,
            greedy,
            first: groups_before + 1,
            count: self.groups_seen - groups_before,
        })
    }

    /// A one-character atom spelled `p[start..self.i]`.
    fn char_atom(&mut self, start: usize, m: Mods) -> Result<Node, Ineligible> {
        let src = self.p[start..self.i].to_vec();
        Ok(Node::Pred(self.preds.get(&src, m.i)?))
    }

    fn atom(&mut self, m: Mods) -> Result<Node, Ineligible> {
        let start = self.i;
        let ch = self.peek().unwrap();
        match char::from_u32(ch) {
            Some('.') => {
                self.i += 1;
                Ok(Node::Dot { all: m.s })
            }
            Some('(') => self.group(m),
            Some('[') => self.class(m),
            Some('\\') => self.escape(m),
            _ => {
                self.i += 1;
                if m.i {
                    self.char_atom(start, m)
                } else {
                    Ok(Node::Lit(ch))
                }
            }
        }
    }

    fn group(&mut self, m: Mods) -> Result<Node, Ineligible> {
        self.i += 1; // (
        let node = if self.eat('?') {
            if self.eat(':') {
                Node::Group(None, Box::new(self.disjunction(m)?))
            } else {
                // Modifiers: (?ims-ims: ... ). Names were stripped and
                // lookarounds rejected before this point.
                let mut inner = m;
                let mut on = true;
                loop {
                    match self.peek().and_then(char::from_u32) {
                        Some('-') => on = false,
                        Some('i') => inner.i = on,
                        Some('m') => inner.m = on,
                        Some('s') => inner.s = on,
                        Some(':') => break,
                        _ => return Err("unrecognised group"),
                    }
                    self.i += 1;
                }
                self.i += 1; // :
                Node::Group(None, Box::new(self.disjunction(inner)?))
            }
        } else {
            self.groups_seen += 1;
            let idx = self.groups_seen;
            Node::Group(Some(idx), Box::new(self.disjunction(m)?))
        };
        if !self.eat(')') {
            return Err("unterminated group");
        }
        Ok(node)
    }

    fn class(&mut self, m: Mods) -> Result<Node, Ineligible> {
        let start = self.i;
        let mut j = self.i + 1;
        if self.v {
            let mut depth = 1;
            while depth > 0 {
                let ch = *self.p.get(j).ok_or("unterminated class")?;
                if ch == c('\\') {
                    match self.p.get(j + 1).copied() {
                        Some(x) if x == c('q') => return Err("class string disjunction"),
                        Some(x) if x == c('p') || x == c('P') => {
                            if self.string_property(j + 2) {
                                return Err("property of strings");
                            }
                        }
                        _ => {}
                    }
                    j += 2;
                    continue;
                }
                if ch == c('[') {
                    depth += 1;
                } else if ch == c(']') {
                    depth -= 1;
                }
                j += 1;
            }
        } else {
            if self.p.get(j) == Some(&c('^')) {
                j += 1;
            }
            loop {
                let ch = *self.p.get(j).ok_or("unterminated class")?;
                if ch == c(']') {
                    j += 1;
                    break;
                }
                j += if ch == c('\\') { 2 } else { 1 };
            }
        }
        self.i = j;
        self.char_atom(start, m)
    }

    /// Whether `{Name}` at `j` names a property of strings.
    fn string_property(&self, j: usize) -> bool {
        if self.p.get(j) != Some(&c('{')) {
            return false;
        }
        let mut name = String::new();
        let mut k = j + 1;
        while let Some(&ch) = self.p.get(k) {
            if ch == c('}') {
                break;
            }
            name.push(char::from_u32(ch).unwrap_or('\u{FFFD}'));
            k += 1;
        }
        STRING_PROPS.contains(&name.as_str())
    }

    fn is_hex(&self, n: usize) -> bool {
        self.peek_at(n)
            .and_then(char::from_u32)
            .is_some_and(|ch| ch.is_ascii_hexdigit())
    }

    fn escape(&mut self, m: Mods) -> Result<Node, Ineligible> {
        let start = self.i;
        self.i += 1; // backslash
        let e = self.peek().ok_or("trailing backslash")?;
        match char::from_u32(e) {
            Some('1'..='9') => {
                let mut n: usize = 0;
                let mut j = self.i;
                while let Some(d) = self.p.get(j).and_then(|&x| char::from_u32(x)).and_then(|x| x.to_digit(10)) {
                    n = n.saturating_mul(10).saturating_add(d as usize);
                    j += 1;
                }
                if n <= self.ngroups || self.u {
                    return Err("backreference");
                }
                self.legacy_octal();
            }
            Some('0') => {
                if !self.u && matches!(self.peek_at(1).and_then(char::from_u32), Some('0'..='9')) {
                    self.legacy_octal();
                } else {
                    self.i += 1;
                }
            }
            Some('k') if self.u => return Err("named reference"),
            Some('p') | Some('P') if self.u => {
                if self.v && self.string_property(self.i + 1) {
                    return Err("property of strings");
                }
                while let Some(ch) = self.peek() {
                    self.i += 1;
                    if ch == c('}') {
                        break;
                    }
                }
            }
            Some('c') => {
                if self.peek_at(1).and_then(char::from_u32).is_some_and(|l| l.is_ascii_alphabetic()) {
                    self.i += 2;
                } else if self.u {
                    return Err("invalid escape");
                } else {
                    // Annex B: a lone `\` before a `c` that starts the next atom.
                    let src = [c('\\'), c('\\')];
                    return Ok(if m.i {
                        Node::Pred(self.preds.get(&src, true)?)
                    } else {
                        Node::Lit(c('\\'))
                    });
                }
            }
            Some('x') => {
                self.i += 1;
                if self.is_hex(0) && self.is_hex(1) {
                    self.i += 2;
                }
            }
            Some('u') => {
                self.i += 1;
                if !self.u && self.at('{') {
                    // Annex B reads `\u{2}` as `u{2}`; the backtracking
                    // matcher reads a code point escape.
                    return Err("braced unicode escape outside unicode mode");
                }
                if self.u && self.at('{') {
                    while let Some(ch) = self.peek() {
                        self.i += 1;
                        if ch == c('}') {
                            break;
                        }
                    }
                } else if (0..4).all(|k| self.is_hex(k)) {
                    let hex = |p: &Self, from: usize| -> u32 {
                        (0..4).fold(0, |v, k| {
                            v * 16 + char::from_u32(p.p[from + k]).and_then(|x| x.to_digit(16)).unwrap()
                        })
                    };
                    let lead = hex(self, self.i);
                    self.i += 4;
                    // A surrogate pair spelled as two escapes is one code
                    // point in unicode mode.
                    if self.u
                        && (0xD800..0xDC00).contains(&lead)
                        && self.at('\\')
                        && self.peek_at(1) == Some(c('u'))
                        && (2..6).all(|k| self.is_hex(k))
                    {
                        let trail = hex(self, self.i + 2);
                        if (0xDC00..0xE000).contains(&trail) {
                            self.i += 6;
                        }
                    }
                }
            }
            _ => {
                self.i += 1;
            }
        }
        // Assertions (`\b`, `\B`) were handled by `term`; everything left
        // is one character.
        self.char_atom(start, m)
    }

    /// Annex B LegacyOctalEscapeSequence (or `\8`, `\9` identity) at the
    /// cursor, which sits on the first digit.
    fn legacy_octal(&mut self) {
        let d0 = self.peek().unwrap();
        self.i += 1;
        if d0 == c('8') || d0 == c('9') {
            return;
        }
        let is_oct = |x: Option<u32>| matches!(x, Some(x) if (c('0')..=c('7')).contains(&x));
        if is_oct(self.peek()) {
            let two = (d0 - c('0')) * 8 + (self.peek().unwrap() - c('0'));
            self.i += 1;
            if two <= 0o37 && is_oct(self.peek()) {
                self.i += 1;
            }
        }
    }
}

// ---- program -------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Inst {
    Lit(u32),
    Pred(u32),
    Any,
    AnyNoLt,
    Split(u32, u32),
    Jmp(u32),
    Save(u32),
    /// Unset capture slots `lo..hi`.
    Reset(u32, u32),
    Start { multiline: bool },
    End { multiline: bool },
    Word { invert: bool, pred: u32 },
    /// Begin an iteration of a quantifier with a nullable body at this
    /// nesting level: clear the level's consumed bit.
    Enter(u8),
    /// End such an iteration: fail unless something was consumed.
    Check(u8),
    Match,
}

struct Compiler {
    prog: Vec<Inst>,
    /// Nesting level of nullable-body iterations at each instruction.
    depth_at: Vec<u8>,
    depth: u8,
    max_depth: u8,
}

impl Compiler {
    fn emit(&mut self, i: Inst) -> Result<usize, Ineligible> {
        if self.prog.len() >= MAX_PROGRAM {
            return Err("program too large");
        }
        self.prog.push(i);
        self.depth_at.push(self.depth);
        Ok(self.prog.len() - 1)
    }

    fn pc(&self) -> u32 {
        self.prog.len() as u32
    }

    fn node(&mut self, n: &Node) -> Result<(), Ineligible> {
        match n {
            Node::Empty => {}
            Node::Lit(x) => {
                self.emit(Inst::Lit(*x))?;
            }
            Node::Pred(p) => {
                self.emit(Inst::Pred(*p))?;
            }
            Node::Dot { all } => {
                self.emit(if *all { Inst::Any } else { Inst::AnyNoLt })?;
            }
            Node::Start { multiline } => {
                self.emit(Inst::Start { multiline: *multiline })?;
            }
            Node::End { multiline } => {
                self.emit(Inst::End { multiline: *multiline })?;
            }
            Node::Word { invert, pred } => {
                self.emit(Inst::Word { invert: *invert, pred: *pred })?;
            }
            Node::Group(idx, body) => {
                if let Some(k) = idx {
                    self.emit(Inst::Save((2 * k) as u32))?;
                }
                self.node(body)?;
                if let Some(k) = idx {
                    self.emit(Inst::Save((2 * k + 1) as u32))?;
                }
            }
            Node::Concat(v) => {
                for x in v {
                    self.node(x)?;
                }
            }
            Node::Alt(v) => {
                let mut jumps = Vec::new();
                for (k, x) in v.iter().enumerate() {
                    if k + 1 < v.len() {
                        let split = self.emit(Inst::Split(0, 0))?;
                        self.node(x)?;
                        jumps.push(self.emit(Inst::Jmp(0))?);
                        let next = self.pc();
                        self.prog[split] = Inst::Split(split as u32 + 1, next);
                    } else {
                        self.node(x)?;
                    }
                }
                let end = self.pc();
                for j in jumps {
                    self.prog[j] = Inst::Jmp(end);
                }
            }
            Node::Repeat { body, min, max, greedy, first, count } => {
                if *min as usize > MAX_PROGRAM || max.is_some_and(|m| (m - min) as usize > MAX_PROGRAM) {
                    return Err("quantifier bounds too large to unroll");
                }
                let reset = (*count > 0).then(|| Inst::Reset((2 * first) as u32, (2 * (first + count)) as u32));
                for _ in 0..*min {
                    if let Some(r) = reset {
                        self.emit(r)?;
                    }
                    self.node(body)?;
                }
                let check = nullable(body);
                let optional = |c: &mut Compiler| -> Result<(), Ineligible> {
                    if check {
                        if c.depth as u32 >= MAX_EMPTY_DEPTH {
                            return Err("nullable quantifiers nested too deeply");
                        }
                        c.depth += 1;
                        c.max_depth = c.max_depth.max(c.depth);
                        c.emit(Inst::Enter(c.depth - 1))?;
                    }
                    if let Some(r) = reset {
                        c.emit(r)?;
                    }
                    c.node(body)?;
                    if check {
                        c.emit(Inst::Check(c.depth - 1))?;
                        c.depth -= 1;
                    }
                    Ok(())
                };
                let split = |a: u32, b: u32| if *greedy { Inst::Split(a, b) } else { Inst::Split(b, a) };
                match max {
                    None => {
                        let head = self.emit(Inst::Split(0, 0))?;
                        optional(self)?;
                        self.emit(Inst::Jmp(head as u32))?;
                        let exit = self.pc();
                        self.prog[head] = split(head as u32 + 1, exit);
                    }
                    Some(max) => {
                        let mut splits = Vec::new();
                        for _ in *min..*max {
                            splits.push(self.emit(Inst::Split(0, 0))?);
                            optional(self)?;
                        }
                        let exit = self.pc();
                        for s in splits {
                            self.prog[s] = split(s as u32 + 1, exit);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// How many backtracking steps one linear-matcher step is worth, for the
/// auto routing rule (measured with gable-js-bench's regex workloads).
const LINEAR_STEP_COST: u64 = 16;

/// Worst-case cost facts for the backtracking matcher, for the auto
/// routing rule (see [`Linear::backtracking_is_bounded`]).
#[derive(Clone, Copy, Debug, Default)]
struct Cost {
    /// A repeated atom contains a quantifier or an alternation: the
    /// backtracking matcher can be exponential.
    nested: bool,
    /// Unbounded quantifiers (each multiplies the worst case by the input
    /// length).
    unbounded: u32,
    /// Product of the bounded quantifiers' choice counts and the
    /// alternations' widths.
    choices: u64,
    /// Every match must begin at input start (a leading non-multiline
    /// `^` in each alternative), so a failed attempt anywhere else ends at
    /// its first instruction.
    anchored: bool,
}

/// Whether every path through `n` begins with a non-multiline `^`.
fn anchored(n: &Node) -> bool {
    match n {
        Node::Start { multiline: false } => true,
        Node::Group(_, b) => anchored(b),
        Node::Concat(v) => v.first().is_some_and(anchored),
        Node::Alt(v) => v.iter().all(anchored),
        _ => false,
    }
}

fn cost(n: &Node, acc: &mut Cost) {
    fn repeats_or_alts(n: &Node) -> bool {
        match n {
            Node::Alt(_) => true,
            Node::Repeat { max, .. } => max.map_or(true, |m| m > 1),
            Node::Group(_, b) => repeats_or_alts(b),
            Node::Concat(v) => v.iter().any(repeats_or_alts),
            _ => false,
        }
    }
    match n {
        Node::Group(_, b) => cost(b, acc),
        Node::Concat(v) => v.iter().for_each(|x| cost(x, acc)),
        Node::Alt(v) => {
            acc.choices = acc.choices.saturating_mul(v.len() as u64);
            v.iter().for_each(|x| cost(x, acc));
        }
        Node::Repeat { body, min, max, .. } => {
            if max.map_or(true, |m| m > 1) && repeats_or_alts(body) {
                acc.nested = true;
            }
            match max {
                None => acc.unbounded += 1,
                Some(m) => acc.choices = acc.choices.saturating_mul((m - min) as u64 + 1),
            }
            cost(body, acc);
        }
        _ => {}
    }
}

// ---- thread lists ----------------------------------------------------------------------------

struct Threads {
    dense: Vec<u32>,
    sparse: Vec<u32>,
    len: usize,
    /// `nslots` capture slots per state.
    slots: Vec<usize>,
}

impl Threads {
    fn new(states: usize, nslots: usize) -> Threads {
        Threads {
            dense: vec![0; states],
            sparse: vec![0; states],
            len: 0,
            slots: vec![NONE; states * nslots],
        }
    }
    fn contains(&self, id: u32) -> bool {
        let s = self.sparse[id as usize] as usize;
        s < self.len && self.dense[s] == id
    }
    fn insert(&mut self, id: u32) {
        self.sparse[id as usize] = self.len as u32;
        self.dense[self.len] = id;
        self.len += 1;
    }
}

enum Frame {
    Explore(u32, u32),
    Restore(u32, usize),
}

struct Scratch {
    clist: Threads,
    nlist: Threads,
    slots: Vec<usize>,
    stack: Vec<Frame>,
}

// ---- the matcher -----------------------------------------------------------------------------

/// A pattern compiled for the linear-time matcher.
pub(crate) struct Linear {
    prog: Vec<Inst>,
    /// Bits of the consumed mask that matter at each instruction.
    relevant: Vec<u32>,
    /// log2 of the states per instruction.
    shift: u32,
    nslots: usize,
    preds: Vec<Pred>,
    unicode: bool,
    cost: Cost,
    /// The instructions that can consume the first character of a match,
    /// when no match can be empty: used to skip ahead between attempts.
    first: Option<Vec<Inst>>,
    scratch: RefCell<Option<Scratch>>,
}

/// Whether the pattern spells `\uXXXX\uXXXX` with a lead surrogate first
/// (escapes are not decoded otherwise; a false positive only keeps the
/// pattern on the backtracking matcher).
fn escaped_surrogate_pair(p: &[u32]) -> bool {
    let hex4 = |i: usize| -> Option<u32> {
        if p.get(i) != Some(&c('\\')) || p.get(i + 1) != Some(&c('u')) {
            return None;
        }
        (0..4).try_fold(0u32, |v, k| Some(v * 16 + char::from_u32(*p.get(i + 2 + k)?)?.to_digit(16)?))
    };
    (0..p.len()).any(|i| {
        matches!(hex4(i), Some(h) if (0xD800..0xDC00).contains(&h))
            && hex4(i + 6).is_some()
    })
}

/// Whether a code unit is a LineTerminator.
fn is_lt(u: u32) -> bool {
    matches!(u, 0x0A | 0x0D | 0x2028 | 0x2029)
}

impl Linear {
    /// Compile `pattern` (the numbered-groups pattern the backtracking
    /// matcher also sees, already validated) or say why it is not eligible.
    pub(crate) fn compile(
        pattern: &[u32],
        ngroups: usize,
        icase: bool,
        multiline: bool,
        dot_all: bool,
        unicode: bool,
        unicode_sets: bool,
    ) -> Result<Linear, Ineligible> {
        if !(unicode || unicode_sets) && escaped_surrogate_pair(pattern) {
            // The backtracking matcher joins `\uD83D\uDE00` into one code
            // point even outside unicode mode, so it never matches the two
            // code units the spec says it should. Leave such patterns where
            // they are so both modes keep agreeing. An escaped lead before
            // any other four-digit escape goes wrong too: the lead followed
            // by an optional escaped `b` misses the lone lead in the input
            // `a`, lead, trail.
            return Err("escaped lead surrogate before another escape outside unicode mode");
        }
        let mut p = Parser {
            p: pattern,
            i: 0,
            u: unicode || unicode_sets,
            v: unicode_sets,
            ngroups,
            groups_seen: 0,
            preds: PredTable { preds: Vec::new(), index: HashMap::new(), unicode, unicode_sets },
        };
        let ast = p.disjunction(Mods { i: icase, m: multiline, s: dot_all })?;
        if p.i != pattern.len() || p.groups_seen != ngroups {
            return Err("pattern not fully understood");
        }
        let mut cmp = Compiler { prog: Vec::new(), depth_at: Vec::new(), depth: 0, max_depth: 0 };
        cmp.emit(Inst::Save(0))?;
        cmp.node(&ast)?;
        cmp.emit(Inst::Save(1))?;
        cmp.emit(Inst::Match)?;
        let shift = cmp.max_depth as u32;
        let nslots = 2 * (ngroups + 1);
        let states = cmp.prog.len() << shift;
        if states.saturating_mul(nslots) > MAX_TABLE {
            return Err("thread table too large");
        }
        let relevant = cmp.depth_at.iter().map(|&d| (1u32 << d) - 1).collect();
        let mut c = Cost { choices: 1, ..Cost::default() };
        cost(&ast, &mut c);
        c.anchored = anchored(&ast);
        let mut lin = Linear {
            prog: cmp.prog,
            relevant,
            shift,
            nslots,
            preds: p.preds.preds,
            unicode: unicode || unicode_sets,
            cost: c,
            first: None,
            scratch: RefCell::new(None),
        };
        lin.first = lin.first_set();
        Ok(lin)
    }

    /// Program size, for the routing estimate.
    fn size(&self) -> u64 {
        self.prog.len() as u64
    }

    /// The auto rule: whether the backtracking matcher's worst case on
    /// `len` units of input from `starts` start positions stays under
    /// `budget` steps, so the (usually faster) backtracker is safe.
    pub(crate) fn backtracking_is_bounded(&self, len: usize, starts: usize, budget: u64) -> bool {
        let c = &self.cost;
        if c.nested {
            return false;
        }
        let per_path = self.size().saturating_mul(c.choices);
        if c.unbounded == 0 {
            // No unbounded loop: every attempt is bounded by the pattern,
            // so the whole search is linear in the input already.
            return per_path <= budget;
        }
        let starts = if c.anchored { 1 } else { starts };
        let mut est = per_path.saturating_mul(starts as u64);
        for _ in 0..c.unbounded {
            est = est.saturating_mul(len as u64 + 1);
        }
        // The linear matcher's own cost is about `size × len` steps, each
        // dearer than a backtracking step. Backtracking that is within a
        // constant of that (one loop under `^`, say) stays: switching
        // matchers only pays when backtracking grows faster than the input.
        let linear = self.size().saturating_mul(len as u64 + 1).saturating_mul(LINEAR_STEP_COST);
        est <= budget.max(linear)
    }

    /// Epsilon closure of the entry point, ignoring assertions: the
    /// consuming instructions a match can begin with.
    fn first_set(&self) -> Option<Vec<Inst>> {
        let mut seen = vec![false; self.prog.len()];
        let mut stack = vec![0usize];
        let mut out = Vec::new();
        while let Some(pc) = stack.pop() {
            if std::mem::replace(&mut seen[pc], true) {
                continue;
            }
            match self.prog[pc] {
                Inst::Lit(_) | Inst::Pred(_) | Inst::Any | Inst::AnyNoLt => out.push(self.prog[pc]),
                Inst::Match => return None,
                Inst::Split(a, b) => {
                    stack.push(b as usize);
                    stack.push(a as usize);
                }
                Inst::Jmp(t) => stack.push(t as usize),
                _ => stack.push(pc + 1),
            }
        }
        if out.iter().any(|i| matches!(i, Inst::Any)) || out.len() > 8 {
            return None;
        }
        Some(out)
    }

    /// The character at `pos` and its length in code units.
    #[inline]
    fn decode(&self, input: &[u16], pos: usize) -> Option<(u32, usize)> {
        let u = *input.get(pos)? as u32;
        if self.unicode && (0xD800..0xDC00).contains(&u) {
            if let Some(&l) = input.get(pos + 1) {
                let l = l as u32;
                if (0xDC00..0xE000).contains(&l) {
                    return Some((0x10000 + ((u - 0xD800) << 10) + (l - 0xDC00), 2));
                }
            }
        }
        Some((u, 1))
    }

    fn decode_left(&self, input: &[u16], pos: usize) -> Option<u32> {
        if pos == 0 {
            return None;
        }
        let u = input[pos - 1] as u32;
        if self.unicode && (0xDC00..0xE000).contains(&u) && pos >= 2 {
            let h = input[pos - 2] as u32;
            if (0xD800..0xDC00).contains(&h) {
                return Some(0x10000 + ((h - 0xD800) << 10) + (u - 0xDC00));
            }
        }
        Some(u)
    }

    #[inline]
    fn consumes(&self, inst: Inst, ch: u32) -> bool {
        match inst {
            Inst::Lit(x) => x == ch,
            Inst::Pred(p) => self.preds[p as usize].test(ch),
            Inst::Any => true,
            Inst::AnyNoLt => !is_lt(ch),
            _ => false,
        }
    }

    /// Find the leftmost match at or after `start` (or exactly at `start`
    /// when `sticky`). Returns the capture slots as ranges.
    pub(crate) fn exec(&self, input: &[u16], start: usize, sticky: bool) -> Option<Vec<Option<(usize, usize)>>> {
        if start > input.len() {
            return None;
        }
        let mut guard = self.scratch.borrow_mut();
        let s = guard.get_or_insert_with(|| {
            let states = self.prog.len() << self.shift;
            Scratch {
                clist: Threads::new(states, self.nslots),
                nlist: Threads::new(states, self.nslots),
                slots: vec![NONE; self.nslots],
                stack: Vec::new(),
            }
        });
        s.clist.len = 0;
        s.nlist.len = 0;
        let all = u32::MAX;
        let mut best: Option<Vec<usize>> = None;
        let mut pos = start;
        loop {
            if best.is_none() && (!sticky || pos == start) {
                if s.clist.len == 0 && !sticky {
                    pos = self.skip(input, pos);
                }
                s.slots.iter_mut().for_each(|x| *x = NONE);
                self.add(&mut s.clist, &mut s.slots, &mut s.stack, 0, 0, input, pos);
            }
            if s.clist.len == 0 {
                break;
            }
            let here = self.decode(input, pos);
            for k in 0..s.clist.len {
                let id = s.clist.dense[k];
                let pc = (id >> self.shift) as usize;
                let inst = self.prog[pc];
                if let Inst::Match = inst {
                    let base = id as usize * self.nslots;
                    best = Some(s.clist.slots[base..base + self.nslots].to_vec());
                    break;
                }
                if let Some((ch, w)) = here {
                    if self.consumes(inst, ch) {
                        let base = id as usize * self.nslots;
                        s.slots.copy_from_slice(&s.clist.slots[base..base + self.nslots]);
                        self.add(&mut s.nlist, &mut s.slots, &mut s.stack, pc + 1, all, input, pos + w);
                    }
                }
            }
            std::mem::swap(&mut s.clist, &mut s.nlist);
            s.nlist.len = 0;
            match here {
                Some((_, w)) => pos += w,
                None => break,
            }
            if sticky && best.is_none() && s.clist.len == 0 {
                break;
            }
        }
        let b = best?;
        Some(
            (0..self.nslots / 2)
                .map(|g| match (b[2 * g], b[2 * g + 1]) {
                    (x, y) if x != NONE && y != NONE => Some((x, y)),
                    _ => None,
                })
                .collect(),
        )
    }

    /// With no thread alive, advance to the next position where a match
    /// could begin.
    fn skip(&self, input: &[u16], mut pos: usize) -> usize {
        let Some(first) = &self.first else { return pos };
        while let Some((ch, w)) = self.decode(input, pos) {
            if first.iter().any(|&i| self.consumes(i, ch)) {
                return pos;
            }
            pos += w;
        }
        pos
    }

    /// Add the thread at `pc` and everything it reaches without consuming,
    /// in priority order, to `list`.
    #[allow(clippy::too_many_arguments)]
    fn add(
        &self,
        list: &mut Threads,
        slots: &mut [usize],
        stack: &mut Vec<Frame>,
        pc: usize,
        mask: u32,
        input: &[u16],
        pos: usize,
    ) {
        stack.push(Frame::Explore(pc as u32, mask));
        while let Some(f) = stack.pop() {
            let (mut pc, mut mask) = match f {
                Frame::Restore(slot, v) => {
                    slots[slot as usize] = v;
                    continue;
                }
                Frame::Explore(pc, mask) => (pc as usize, mask),
            };
            loop {
                mask &= self.relevant[pc];
                let id = ((pc as u32) << self.shift) | mask;
                if list.contains(id) {
                    break;
                }
                list.insert(id);
                match self.prog[pc] {
                    Inst::Jmp(t) => pc = t as usize,
                    Inst::Split(a, b) => {
                        stack.push(Frame::Explore(b, mask));
                        pc = a as usize;
                    }
                    Inst::Save(k) => {
                        stack.push(Frame::Restore(k, slots[k as usize]));
                        slots[k as usize] = pos;
                        pc += 1;
                    }
                    Inst::Reset(lo, hi) => {
                        for k in lo..hi {
                            stack.push(Frame::Restore(k, slots[k as usize]));
                            slots[k as usize] = NONE;
                        }
                        pc += 1;
                    }
                    Inst::Start { multiline } => {
                        let ok = pos == 0 || (multiline && is_lt(input[pos - 1] as u32));
                        if !ok {
                            break;
                        }
                        pc += 1;
                    }
                    Inst::End { multiline } => {
                        let ok = pos == input.len() || (multiline && is_lt(input[pos] as u32));
                        if !ok {
                            break;
                        }
                        pc += 1;
                    }
                    Inst::Word { invert, pred } => {
                        let p = &self.preds[pred as usize];
                        let a = self.decode_left(input, pos).is_some_and(|c| p.test(c));
                        let b = self.decode(input, pos).is_some_and(|(c, _)| p.test(c));
                        if (a != b) == invert {
                            break;
                        }
                        pc += 1;
                    }
                    Inst::Enter(d) => {
                        mask &= !(1 << d);
                        pc += 1;
                    }
                    Inst::Check(d) => {
                        if mask & (1 << d) == 0 {
                            break;
                        }
                        pc += 1;
                    }
                    Inst::Lit(_) | Inst::Pred(_) | Inst::Any | Inst::AnyNoLt | Inst::Match => {
                        let base = id as usize * self.nslots;
                        list.slots[base..base + self.nslots].copy_from_slice(slots);
                        break;
                    }
                }
            }
        }
    }
}
