//! The regex parser (TASK-100.31): a pattern that `syntax` has accepted,
//! turned into the tree both matchers compile from.
//!
//! Everything the ECMAScript semantics decides at pattern-compile time is
//! decided here, so the matchers never look at flags for it:
//!
//! - group names are resolved (`\k<name>` becomes a backreference to every
//!   group of that name, of which at most one participates);
//! - modifiers `(?ims-ims:)` are applied to the nodes they scope;
//! - each character atom, escape and class becomes one [`CharSet`], case
//!   closed under the i flag (so matching is plain membership) and
//!   complemented where the class is negated, with the v-mode order of
//!   folding and complementing;
//! - v-mode classes that contain strings become an alternation of the
//!   strings, longest first, then the single characters;
//! - Annex B legacy forms (octal escapes, `\c`, identity escapes, literal
//!   braces, quantified lookahead, `\u{` outside unicode mode) are read as
//!   Annex B says.

use std::collections::BTreeSet;

use crate::charset::{CharSet, MAX_CODE_POINT};
use crate::unicode;

/// A parsed pattern.
#[derive(Debug)]
pub(crate) struct Pattern {
    pub node: Node,
    /// Capture-group names by group number (group `n` at index `n - 1`).
    pub names: Vec<Option<String>>,
    /// Unicode mode (u or v): the input is read as code points.
    pub unicode: bool,
}

impl Pattern {
    pub(crate) fn group_count(&self) -> usize {
        self.names.len()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Assertion {
    /// `^`: input start, or after a line terminator under m.
    Start { multiline: bool },
    /// `$`: input end, or before a line terminator under m.
    End { multiline: bool },
    /// `\b` (or `\B` when `negate`); `extended` adds the characters that
    /// case-fold into the word characters (unicode mode with i).
    Word { negate: bool, extended: bool },
}

#[derive(Clone, Debug)]
pub(crate) enum Node {
    Empty,
    /// One character from the set.
    Set(CharSet),
    Assert(Assertion),
    /// Capture group `n` (1-based).
    Capture(usize, Box<Node>),
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat {
        body: Box<Node>,
        min: u32,
        /// `None` is unbounded.
        max: Option<u32>,
        greedy: bool,
        /// Capture groups inside the body: `first..first + count`.
        first: usize,
        count: usize,
    },
    /// A backreference to the first of these groups that is set (several
    /// for a duplicated name).
    BackRef {
        groups: Vec<usize>,
        icase: bool,
    },
    Look {
        behind: bool,
        negate: bool,
        body: Box<Node>,
    },
}

impl Node {
    /// Whether the node can match without consuming input.
    #[cfg_attr(not(feature = "linear"), allow(dead_code))]
    pub(crate) fn nullable(&self) -> bool {
        match self {
            Node::Empty | Node::Assert(_) | Node::Look { .. } | Node::BackRef { .. } => true,
            Node::Set(_) => false,
            Node::Capture(_, b) => b.nullable(),
            Node::Concat(v) => v.iter().all(Node::nullable),
            Node::Alt(v) => v.iter().any(Node::nullable),
            Node::Repeat { body, min, .. } => *min == 0 || body.nullable(),
        }
    }
}

#[derive(Clone, Copy)]
struct Mods {
    i: bool,
    m: bool,
    s: bool,
}

fn c(ch: char) -> u32 {
    ch as u32
}

/// Line terminators, which `.` does not match without s.
fn line_terminators() -> CharSet {
    CharSet::from_unsorted(vec![(0x0A, 0x0A), (0x0D, 0x0D), (0x2028, 0x2029)])
}

fn digits() -> CharSet {
    CharSet::range(c('0'), c('9'))
}

fn white_space() -> CharSet {
    CharSet::from_unsorted(vec![
        (0x09, 0x0D),
        (0x20, 0x20),
        (0xA0, 0xA0),
        (0x1680, 0x1680),
        (0x2000, 0x200A),
        (0x2028, 0x2029),
        (0x202F, 0x202F),
        (0x205F, 0x205F),
        (0x3000, 0x3000),
        (0xFEFF, 0xFEFF),
    ])
}

/// WordCharacters(rer): `[A-Za-z0-9_]`, plus U+017F and U+212A (which fold
/// into it) when `extended` (unicode mode with i).
pub(crate) fn word_chars(extended: bool) -> CharSet {
    let mut v = vec![
        (c('0'), c('9')),
        (c('A'), c('Z')),
        (c('_'), c('_')),
        (c('a'), c('z')),
    ];
    if extended {
        v.push((0x17F, 0x17F));
        v.push((0x212A, 0x212A));
    }
    CharSet::from_unsorted(v)
}

/// A class's value in v mode: single characters plus strings of any other
/// length (folded under i, so that set operations compare folded strings).
#[derive(Clone, Default)]
struct ClassValue {
    chars: CharSet,
    strings: BTreeSet<Vec<u32>>,
}

impl ClassValue {
    fn chars(chars: CharSet) -> ClassValue {
        ClassValue {
            chars,
            strings: BTreeSet::new(),
        }
    }
    fn union(&mut self, o: ClassValue) {
        self.chars = self.chars.union(&o.chars);
        self.strings.extend(o.strings);
    }
    fn intersect(&mut self, o: &ClassValue) {
        self.chars = self.chars.intersect(&o.chars);
        self.strings.retain(|s| o.strings.contains(s));
    }
    fn subtract(&mut self, o: &ClassValue) {
        self.chars = self.chars.subtract(&o.chars);
        self.strings.retain(|s| !o.strings.contains(s));
    }
}

struct Parser<'a> {
    p: &'a [u32],
    i: usize,
    u: bool,
    v: bool,
    /// The NamedCaptureGroups parameter: `\k` is a reference.
    named: bool,
    names: &'a [Option<String>],
    groups_seen: usize,
}

/// Parse `pattern` (code points in unicode mode, code units otherwise),
/// which the grammar check has accepted.
pub(crate) fn parse(
    pattern: &[u32],
    icase: bool,
    multiline: bool,
    dot_all: bool,
    unicode: bool,
    sets: bool,
) -> Result<Pattern, String> {
    let u = unicode || sets;
    let names = group_names(pattern, sets);
    let mut p = Parser {
        p: pattern,
        i: 0,
        u,
        v: sets,
        named: u || names.iter().any(Option::is_some),
        names: &names,
        groups_seen: 0,
    };
    let node = p.disjunction(Mods {
        i: icase,
        m: multiline,
        s: dot_all,
    })?;
    if p.i != pattern.len() || p.groups_seen != names.len() {
        return Err("Invalid regular expression: unexpected parse state".into());
    }
    Ok(Pattern {
        node,
        names,
        unicode: u,
    })
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
    fn at_str(&self, s: &str) -> bool {
        s.chars()
            .enumerate()
            .all(|(k, ch)| self.peek_at(k) == Some(c(ch)))
    }
    fn eat(&mut self, ch: char) -> bool {
        if self.at(ch) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn fail<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("Invalid regular expression: {what}"))
    }
    fn digit_at(&self, n: usize) -> Option<u32> {
        self.peek_at(n)
            .and_then(char::from_u32)
            .and_then(|ch| ch.to_digit(10))
    }
    fn hex_at(&self, n: usize) -> Option<u32> {
        self.peek_at(n)
            .and_then(char::from_u32)
            .and_then(|ch| ch.to_digit(16))
    }

    /// The set for one literal character under the current modifiers.
    fn literal(&self, ch: u32, m: Mods) -> CharSet {
        if m.i {
            unicode::case_class(ch, self.u)
        } else {
            CharSet::single(ch)
        }
    }

    /// A set built from raw members (an escape or a non-v class), closed
    /// under the i flag.
    fn closed(&self, set: CharSet, m: Mods) -> CharSet {
        if m.i {
            unicode::case_closure(&set, self.u)
        } else {
            set
        }
    }

    // ---- structure -------------------------------------------------------------------------

    fn disjunction(&mut self, m: Mods) -> Result<Node, String> {
        let mut alts = vec![self.alternative(m)?];
        while self.eat('|') {
            alts.push(self.alternative(m)?);
        }
        Ok(if alts.len() == 1 {
            alts.pop().unwrap()
        } else {
            Node::Alt(alts)
        })
    }

    fn alternative(&mut self, m: Mods) -> Result<Node, String> {
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

    fn term(&mut self, m: Mods) -> Result<Node, String> {
        if crate::stack::exhausted() {
            return self.fail(crate::stack::EXHAUSTED_MESSAGE);
        }
        let ch = self.peek().unwrap();
        if ch == c('^') {
            self.i += 1;
            return Ok(Node::Assert(Assertion::Start { multiline: m.m }));
        }
        if ch == c('$') {
            self.i += 1;
            return Ok(Node::Assert(Assertion::End { multiline: m.m }));
        }
        if ch == c('\\') && matches!(self.peek_at(1), Some(x) if x == c('b') || x == c('B')) {
            let negate = self.peek_at(1) == Some(c('B'));
            self.i += 2;
            return Ok(Node::Assert(Assertion::Word {
                negate,
                extended: m.i && self.u,
            }));
        }
        let groups_before = self.groups_seen;
        let lookahead = self.at_str("(?=") || self.at_str("(?!");
        let lookbehind = self.at_str("(?<=") || self.at_str("(?<!");
        if lookahead || lookbehind {
            let negate = self.peek_at(if lookahead { 2 } else { 3 }) == Some(c('!'));
            self.i += if lookahead { 3 } else { 4 };
            let body = self.disjunction(m)?;
            if !self.eat(')') {
                return self.fail("unterminated group");
            }
            let node = Node::Look {
                behind: lookbehind,
                negate,
                body: Box::new(body),
            };
            // Annex B: a lookahead may be quantified outside unicode mode.
            return if lookahead && !self.u {
                self.quantifier(node, groups_before)
            } else {
                Ok(node)
            };
        }
        let atom = self.atom(m)?;
        self.quantifier(atom, groups_before)
    }

    /// A braced quantifier at the cursor: its length and bounds (saturated
    /// to `u32::MAX`).
    fn braced(&self) -> Option<(usize, u32, Option<u32>)> {
        let mut j = self.i + 1;
        let number = |j: &mut usize| -> Option<u32> {
            let start = *j;
            let mut v: u64 = 0;
            while let Some(d) = self
                .p
                .get(*j)
                .and_then(|&x| char::from_u32(x))
                .and_then(|x| x.to_digit(10))
            {
                v = (v * 10 + d as u64).min(u32::MAX as u64);
                *j += 1;
            }
            (*j != start).then_some(v as u32)
        };
        let min = number(&mut j)?;
        let mut max = Some(min);
        if self.p.get(j) == Some(&c(',')) {
            j += 1;
            max = number(&mut j);
        }
        if self.p.get(j) != Some(&c('}')) {
            return None;
        }
        Some((j + 1 - self.i, min, max))
    }

    fn quantifier(&mut self, atom: Node, groups_before: usize) -> Result<Node, String> {
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

    fn atom(&mut self, m: Mods) -> Result<Node, String> {
        let ch = self.peek().unwrap();
        match char::from_u32(ch) {
            Some('.') => {
                self.i += 1;
                let all = CharSet::range(0, MAX_CODE_POINT);
                Ok(Node::Set(if m.s {
                    all
                } else {
                    line_terminators().complement(MAX_CODE_POINT)
                }))
            }
            Some('(') => self.group(m),
            Some('[') => {
                self.i += 1;
                self.class(m)
            }
            Some('\\') => self.atom_escape(m),
            _ => {
                self.i += 1;
                Ok(Node::Set(self.literal(ch, m)))
            }
        }
    }

    fn group(&mut self, m: Mods) -> Result<Node, String> {
        self.i += 1; // (
        let node = if self.eat('?') {
            if self.eat(':') {
                self.disjunction(m)?
            } else if self.eat('<') {
                // A named group; the names were collected up front.
                while !self.eat('>') {
                    self.i += 1;
                }
                self.groups_seen += 1;
                let idx = self.groups_seen;
                Node::Capture(idx, Box::new(self.disjunction(m)?))
            } else {
                let mut inner = m;
                let mut on = true;
                loop {
                    match self.peek().and_then(char::from_u32) {
                        Some('-') => on = false,
                        Some('i') => inner.i = on,
                        Some('m') => inner.m = on,
                        Some('s') => inner.s = on,
                        Some(':') => break,
                        _ => return self.fail("invalid group"),
                    }
                    self.i += 1;
                }
                self.i += 1; // :
                self.disjunction(inner)?
            }
        } else {
            self.groups_seen += 1;
            let idx = self.groups_seen;
            Node::Capture(idx, Box::new(self.disjunction(m)?))
        };
        if !self.eat(')') {
            return self.fail("unterminated group");
        }
        Ok(node)
    }

    // ---- escapes ---------------------------------------------------------------------------

    fn atom_escape(&mut self, m: Mods) -> Result<Node, String> {
        self.i += 1; // backslash
        let Some(e) = self.peek() else {
            return self.fail("\\ at end of pattern");
        };
        match char::from_u32(e) {
            Some('1'..='9') => {
                let start = self.i;
                let mut n: usize = 0;
                while let Some(d) = self.digit_at(0) {
                    n = n.saturating_mul(10).saturating_add(d as usize);
                    self.i += 1;
                }
                if n <= self.names.len() {
                    return Ok(Node::BackRef {
                        groups: vec![n],
                        icase: m.i,
                    });
                }
                // Annex B: an octal or identity escape.
                self.i = start;
                let ch = self.legacy_octal();
                Ok(Node::Set(self.literal(ch, m)))
            }
            Some('k') if self.named => {
                self.i += 1;
                let name = self.reference_name();
                let groups: Vec<usize> = self
                    .names
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| n.as_deref() == Some(name.as_str()))
                    .map(|(k, _)| k + 1)
                    .collect();
                Ok(Node::BackRef { groups, icase: m.i })
            }
            Some('d' | 'D' | 's' | 'S' | 'w' | 'W') => {
                self.i += 1;
                Ok(Node::Set(self.class_escape_set(e, m)))
            }
            Some('p' | 'P') if self.u => {
                self.i += 1;
                let negate = e == c('P');
                let (name, value) = self.property_body();
                if !negate && unicode::is_string_property(&name) {
                    let (chars, strings) = unicode::string_property(&name).unwrap();
                    let mut cv = ClassValue::chars(self.closed(chars, m));
                    for s in strings {
                        cv.strings.insert(self.fold_string(s, m));
                    }
                    return Ok(self.lower_class(cv, m));
                }
                let set = self.property_set(&name, value.as_deref())?;
                Ok(Node::Set(self.property_leaf(set, negate, m)))
            }
            _ => {
                let ch = self.character_escape(false)?;
                Ok(Node::Set(self.literal(ch, m)))
            }
        }
    }

    /// `<name>` after `\k`, decoded.
    fn reference_name(&mut self) -> String {
        let start = self.i + 1;
        let (name, next) = read_group_name(self.p, start);
        self.i = next;
        name
    }

    /// `{Name}` or `{Name=Value}` after `\p` / `\P`.
    fn property_body(&mut self) -> (String, Option<String>) {
        self.i += 1; // {
        let mut body = String::new();
        while let Some(ch) = self.peek() {
            self.i += 1;
            if ch == c('}') {
                break;
            }
            body.push(char::from_u32(ch).unwrap_or('\u{FFFD}'));
        }
        match body.split_once('=') {
            Some((n, v)) => (n.to_string(), Some(v.to_string())),
            None => (body, None),
        }
    }

    fn property_set(&self, name: &str, value: Option<&str>) -> Result<CharSet, String> {
        match unicode::property(name, value) {
            Some(s) => Ok(s),
            None => self.fail("invalid property name"),
        }
    }

    /// `\p{..}` or `\P{..}` as a set: outside v mode the raw set or its
    /// complement, then closed under i; in v mode closed first and then
    /// complemented (MaybeSimpleCaseFolding before CharacterComplement).
    fn property_leaf(&self, set: CharSet, negate: bool, m: Mods) -> CharSet {
        if self.v {
            let s = self.closed(set, m);
            if negate {
                s.complement(MAX_CODE_POINT)
            } else {
                s
            }
        } else {
            let s = if negate {
                set.complement(MAX_CODE_POINT)
            } else {
                set
            };
            self.closed(s, m)
        }
    }

    /// `\d \D \s \S \w \W` as a closed set.
    fn class_escape_set(&self, e: u32, m: Mods) -> CharSet {
        let ch = char::from_u32(e).unwrap();
        let base = match ch.to_ascii_lowercase() {
            'd' => digits(),
            's' => white_space(),
            _ => word_chars(m.i && self.u),
        };
        self.property_leaf(base, ch.is_ascii_uppercase(), m)
    }

    /// Annex B LegacyOctalEscapeSequence, or `\8` / `\9` as identity
    /// escapes, at the first digit.
    fn legacy_octal(&mut self) -> u32 {
        let d0 = self.peek().unwrap();
        self.i += 1;
        if d0 == c('8') || d0 == c('9') {
            return d0;
        }
        let is_oct = |x: Option<u32>| matches!(x, Some(x) if (c('0')..=c('7')).contains(&x));
        let mut v = d0 - c('0');
        if is_oct(self.peek()) {
            v = v * 8 + (self.peek().unwrap() - c('0'));
            self.i += 1;
            if d0 <= c('3') && is_oct(self.peek()) {
                v = v * 8 + (self.peek().unwrap() - c('0'));
                self.i += 1;
            }
        }
        v
    }

    /// A CharacterEscape at the cursor (after the backslash): its value.
    fn character_escape(&mut self, in_class: bool) -> Result<u32, String> {
        let e = self.peek().unwrap();
        let ch = char::from_u32(e);
        let simple = |p: &mut Self, v: u32| {
            p.i += 1;
            Ok(v)
        };
        match ch {
            Some('f') => simple(self, 0x0C),
            Some('n') => simple(self, 0x0A),
            Some('r') => simple(self, 0x0D),
            Some('t') => simple(self, 0x09),
            Some('v') => simple(self, 0x0B),
            Some('c') => {
                let next = self.peek_at(1).and_then(char::from_u32);
                match next {
                    Some(l) if l.is_ascii_alphabetic() => {
                        self.i += 2;
                        Ok(l as u32 % 32)
                    }
                    Some(l) if in_class && !self.u && (l.is_ascii_digit() || l == '_') => {
                        self.i += 2;
                        Ok(l as u32 % 32)
                    }
                    // Annex B: the backslash is literal and `c` is read
                    // as the next atom.
                    _ => Ok(c('\\')),
                }
            }
            Some('x') => {
                self.i += 1;
                match (self.hex_at(0), self.hex_at(1)) {
                    (Some(a), Some(b)) => {
                        self.i += 2;
                        Ok(a * 16 + b)
                    }
                    _ => Ok(c('x')),
                }
            }
            Some('u') => {
                self.i += 1;
                match self.unicode_escape() {
                    Some(v) => Ok(v),
                    None => Ok(c('u')),
                }
            }
            Some('0') if !self.digit_at(1).is_some() || self.u => simple(self, 0),
            Some('0'..='9') if !self.u => Ok(self.legacy_octal()),
            _ => simple(self, e),
        }
    }

    /// After `\u`: `XXXX`, and in unicode mode `{X..}` and an escaped
    /// surrogate pair. `None` (cursor unmoved) when it is no such escape,
    /// which outside unicode mode makes `\u` an identity escape.
    fn unicode_escape(&mut self) -> Option<u32> {
        if self.u && self.at('{') {
            let mut j = 1;
            let mut v = 0u32;
            while let Some(d) = self.hex_at(j) {
                v = v.saturating_mul(16).saturating_add(d);
                j += 1;
            }
            if self.peek_at(j) != Some(c('}')) || j == 1 {
                return None;
            }
            self.i += j + 1;
            return Some(v);
        }
        let hex4 = |p: &Self, from: usize| -> Option<u32> {
            (0..4).try_fold(0u32, |v, k| Some(v * 16 + p.hex_at(from + k)?))
        };
        let lead = hex4(self, 0)?;
        self.i += 4;
        if self.u && (0xD800..0xDC00).contains(&lead) && self.at_str("\\u") {
            if let Some(trail) = hex4(self, 2) {
                if (0xDC00..0xE000).contains(&trail) {
                    self.i += 6;
                    return Some(0x10000 + ((lead - 0xD800) << 10) + (trail - 0xDC00));
                }
            }
        }
        Some(lead)
    }

    // ---- classes ---------------------------------------------------------------------------

    /// After `[`.
    fn class(&mut self, m: Mods) -> Result<Node, String> {
        if self.v {
            let negate = self.eat('^');
            let cv = self.class_set_expression(m)?;
            self.i += 1; // ]
            if negate {
                return Ok(Node::Set(cv.chars.complement(MAX_CODE_POINT)));
            }
            return Ok(self.lower_class(cv, m));
        }
        let negate = self.eat('^');
        let mut ranges = Vec::new();
        loop {
            if self.eat(']') {
                break;
            }
            let a = self.class_atom(m)?;
            if self.at('-') && self.peek_at(1).is_some_and(|x| x != c(']')) {
                let save = self.i;
                self.i += 1;
                let b = self.class_atom(m)?;
                match (&a, &b) {
                    (ClassAtom::Char(x), ClassAtom::Char(y)) => {
                        ranges.push((*x, *y));
                        continue;
                    }
                    _ => {
                        // Annex B: with a class escape on either side the
                        // `-` is literal.
                        self.i = save;
                    }
                }
            }
            match a {
                ClassAtom::Char(x) => ranges.push((x, x)),
                ClassAtom::Set(s) => ranges.extend_from_slice(s.ranges()),
            }
        }
        // Outside v mode the members are raw; the class matches a
        // character when one of them canonicalizes like it.
        let set = self.closed(CharSet::from_unsorted(ranges), m);
        Ok(Node::Set(if negate {
            set.complement(MAX_CODE_POINT)
        } else {
            set
        }))
    }

    /// A ClassAtom outside v mode. Class escapes come back raw (not
    /// closed), since the whole class is closed at the end.
    fn class_atom(&mut self, m: Mods) -> Result<ClassAtom, String> {
        let ch = self.peek().unwrap();
        if ch != c('\\') {
            self.i += 1;
            return Ok(ClassAtom::Char(ch));
        }
        self.i += 1;
        let e = self.peek().unwrap();
        let raw = Mods { i: false, ..m };
        match char::from_u32(e) {
            Some('b') => {
                self.i += 1;
                Ok(ClassAtom::Char(8))
            }
            Some('-') if self.u => {
                self.i += 1;
                Ok(ClassAtom::Char(c('-')))
            }
            Some('d' | 'D' | 's' | 'S' | 'w' | 'W') => {
                self.i += 1;
                // WordCharacters still depends on i (the extras), but no
                // closure yet.
                let ch = char::from_u32(e).unwrap();
                let base = match ch.to_ascii_lowercase() {
                    'd' => digits(),
                    's' => white_space(),
                    _ => word_chars(m.i && self.u),
                };
                Ok(ClassAtom::Set(if ch.is_ascii_uppercase() {
                    base.complement(MAX_CODE_POINT)
                } else {
                    base
                }))
            }
            Some('p' | 'P') if self.u => {
                self.i += 1;
                let (name, value) = self.property_body();
                let set = self.property_set(&name, value.as_deref())?;
                Ok(ClassAtom::Set(self.property_leaf(set, e == c('P'), raw)))
            }
            Some('c') if !self.u => {
                // `\c` with a letter, or in a class a digit or `_`; else a
                // literal backslash with `c` read next.
                let v = self.character_escape(true)?;
                Ok(ClassAtom::Char(v))
            }
            _ => Ok(ClassAtom::Char(self.character_escape(true)?)),
        }
    }

    /// A v-mode ClassSetExpression up to (not including) its `]`.
    fn class_set_expression(&mut self, m: Mods) -> Result<ClassValue, String> {
        if self.at(']') {
            return Ok(ClassValue::default());
        }
        let mut acc = self.class_set_operand_or_range(m)?;
        if self.at_str("&&") {
            while self.at_str("&&") {
                self.i += 2;
                let o = self.class_set_operand(m)?.0;
                acc.intersect(&o);
            }
            return Ok(acc);
        }
        if self.at_str("--") {
            while self.at_str("--") {
                self.i += 2;
                let o = self.class_set_operand(m)?.0;
                acc.subtract(&o);
            }
            return Ok(acc);
        }
        while !self.at(']') {
            let o = self.class_set_operand_or_range(m)?;
            acc.union(o);
        }
        Ok(acc)
    }

    fn class_set_operand_or_range(&mut self, m: Mods) -> Result<ClassValue, String> {
        let (v, ch) = self.class_set_operand(m)?;
        if let Some(lo) = ch {
            if self.at('-') && !self.at_str("--") {
                self.i += 1;
                let hi = self.class_set_character()?;
                return Ok(ClassValue::chars(self.closed(CharSet::range(lo, hi), m)));
            }
        }
        Ok(v)
    }

    /// A ClassSetOperand, closed under i, and its character when it is a
    /// single ClassSetCharacter (which may start a range).
    fn class_set_operand(&mut self, m: Mods) -> Result<(ClassValue, Option<u32>), String> {
        if crate::stack::exhausted() {
            return self.fail(crate::stack::EXHAUSTED_MESSAGE);
        }
        if self.eat('[') {
            let negate = self.eat('^');
            let v = self.class_set_expression(m)?;
            self.i += 1; // ]
            if negate {
                return Ok((ClassValue::chars(v.chars.complement(MAX_CODE_POINT)), None));
            }
            return Ok((v, None));
        }
        if self.at('\\') {
            let e = self.peek_at(1).unwrap_or(0);
            match char::from_u32(e) {
                Some('q') => {
                    self.i += 3; // \q{
                    let mut cv = ClassValue::default();
                    loop {
                        let mut s = Vec::new();
                        while !self.at('|') && !self.at('}') {
                            s.push(self.class_set_character()?);
                        }
                        if s.len() == 1 {
                            cv.chars = cv.chars.union(&self.literal(s[0], m));
                        } else {
                            cv.strings.insert(self.fold_string(s, m));
                        }
                        if self.eat('}') {
                            break;
                        }
                        self.i += 1; // |
                    }
                    return Ok((cv, None));
                }
                Some('d' | 'D' | 's' | 'S' | 'w' | 'W') => {
                    self.i += 2;
                    return Ok((ClassValue::chars(self.class_escape_set(e, m)), None));
                }
                Some('p' | 'P') => {
                    self.i += 2;
                    let (name, value) = self.property_body();
                    if e == c('p') && unicode::is_string_property(&name) {
                        let (chars, strings) = unicode::string_property(&name).unwrap();
                        let mut cv = ClassValue::chars(self.closed(chars, m));
                        for s in strings {
                            cv.strings.insert(self.fold_string(s, m));
                        }
                        return Ok((cv, None));
                    }
                    let set = self.property_set(&name, value.as_deref())?;
                    return Ok((
                        ClassValue::chars(self.property_leaf(set, e == c('P'), m)),
                        None,
                    ));
                }
                _ => {}
            }
        }
        let ch = self.class_set_character()?;
        Ok((ClassValue::chars(self.literal(ch, m)), Some(ch)))
    }

    fn class_set_character(&mut self) -> Result<u32, String> {
        let ch = self.peek().unwrap();
        if ch == c('\\') {
            let e = self.peek_at(1).unwrap_or(0);
            if char::from_u32(e).is_some_and(|x| "&-!#%,:;<=>@`~".contains(x)) {
                self.i += 2;
                return Ok(e);
            }
            if e == c('b') {
                self.i += 2;
                return Ok(8);
            }
            self.i += 1;
            return self.character_escape(false);
        }
        self.i += 1;
        Ok(ch)
    }

    /// A v-mode string as the set operations compare it: simply case
    /// folded under i.
    fn fold_string(&self, s: Vec<u32>, m: Mods) -> Vec<u32> {
        if m.i {
            s.into_iter()
                .map(|x| unicode::canonicalize(x, true))
                .collect()
        } else {
            s
        }
    }

    /// A v-mode class as a node: its strings longest first, then its single
    /// characters, then the empty string if it holds one.
    fn lower_class(&self, cv: ClassValue, m: Mods) -> Node {
        if cv.strings.is_empty() {
            return Node::Set(cv.chars);
        }
        let mut strings: Vec<Vec<u32>> = cv.strings.into_iter().collect();
        strings.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        let mut alts = Vec::new();
        let mut empty = false;
        for s in strings {
            if s.is_empty() {
                empty = true;
                continue;
            }
            alts.push(Node::Concat(
                s.into_iter()
                    .map(|x| Node::Set(self.literal(x, m)))
                    .collect(),
            ));
        }
        if !cv.chars.is_empty() {
            alts.push(Node::Set(cv.chars));
        }
        if empty {
            alts.push(Node::Empty);
        }
        if alts.len() == 1 {
            alts.pop().unwrap()
        } else {
            Node::Alt(alts)
        }
    }
}

enum ClassAtom {
    Char(u32),
    Set(CharSet),
}

/// Capture-group names in group order (`None` for unnamed groups), from a
/// pattern the grammar accepted.
pub(crate) fn group_names(p: &[u32], sets: bool) -> Vec<Option<String>> {
    let mut names = Vec::new();
    let mut i = 0;
    let mut class_depth = 0usize;
    while i < p.len() {
        let ch = p[i];
        if ch == c('\\') {
            i += 2;
            continue;
        }
        if class_depth > 0 {
            if ch == c(']') {
                class_depth -= 1;
            } else if ch == c('[') && sets {
                class_depth += 1;
            }
            i += 1;
            continue;
        }
        if ch == c('[') {
            class_depth = 1;
        } else if ch == c('(') {
            if p.get(i + 1) == Some(&c('?')) {
                if p.get(i + 2) == Some(&c('<'))
                    && !matches!(p.get(i + 3), Some(&x) if x == c('=') || x == c('!'))
                {
                    let (name, next) = read_group_name(p, i + 3);
                    names.push(Some(name));
                    i = next;
                    continue;
                }
            } else {
                names.push(None);
            }
        }
        i += 1;
    }
    names
}

/// A GroupName from `start` (just after `<`) through its `>`: the decoded
/// name and the index after `>`. Escapes and raw surrogate pairs (two
/// units outside unicode mode) are decoded.
pub(crate) fn read_group_name(p: &[u32], start: usize) -> (String, usize) {
    let mut j = start;
    let mut name = String::new();
    let hex = |j: usize, n: usize| -> Option<u32> {
        (0..n).try_fold(0u32, |v, k| {
            Some(v * 16 + char::from_u32(*p.get(j + k)?)?.to_digit(16)?)
        })
    };
    while j < p.len() && p[j] != c('>') {
        let (cp, next) = if p[j] == c('\\') {
            // `\u{X..}` or `\uXXXX`, possibly an escaped surrogate pair.
            if p.get(j + 2) == Some(&c('{')) {
                let mut k = j + 3;
                let mut v = 0u32;
                while k < p.len() && p[k] != c('}') {
                    v = v.saturating_mul(16).saturating_add(
                        char::from_u32(p[k])
                            .and_then(|x| x.to_digit(16))
                            .unwrap_or(0),
                    );
                    k += 1;
                }
                (v, k + 1)
            } else {
                let hi = hex(j + 2, 4).unwrap_or(0);
                match (p.get(j + 6), p.get(j + 7), hex(j + 8, 4)) {
                    (Some(&b), Some(&u), Some(lo))
                        if (0xD800..0xDC00).contains(&hi)
                            && b == c('\\')
                            && u == c('u')
                            && (0xDC00..0xE000).contains(&lo) =>
                    {
                        (0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00), j + 12)
                    }
                    _ => (hi, j + 6),
                }
            }
        } else if (0xD800..0xDC00).contains(&p[j])
            && matches!(p.get(j + 1), Some(&l) if (0xDC00..0xE000).contains(&l))
        {
            (
                0x10000 + ((p[j] - 0xD800) << 10) + (p[j + 1] - 0xDC00),
                j + 2,
            )
        } else {
            (p[j], j + 1)
        };
        if let Some(ch) = char::from_u32(cp) {
            name.push(ch);
        }
        j = next;
    }
    (name, j + 1)
}

/// The pattern as the code points the grammar sees: surrogate pairs join
/// in unicode mode, every code unit stands alone otherwise.
pub(crate) fn pattern_code_points(pattern: &[u16], unicode_mode: bool) -> Vec<u32> {
    let mut out = Vec::with_capacity(pattern.len());
    let mut i = 0;
    while i < pattern.len() {
        let u = pattern[i] as u32;
        if unicode_mode && (0xD800..0xDC00).contains(&u) && i + 1 < pattern.len() {
            let l = pattern[i + 1] as u32;
            if (0xDC00..0xE000).contains(&l) {
                out.push(0x10000 + ((u - 0xD800) << 10) + (l - 0xDC00));
                i += 2;
                continue;
            }
        }
        out.push(u);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(p: &str, flags: &str) -> Pattern {
        let has = |f: char| flags.contains(f);
        let u = has('u') || has('v');
        let units: Vec<u16> = p.encode_utf16().collect();
        crate::syntax::validate_units(&units, has('u'), has('v')).unwrap();
        parse(
            &pattern_code_points(&units, u),
            has('i'),
            has('m'),
            has('s'),
            has('u'),
            has('v'),
        )
        .unwrap()
    }

    fn set_of(n: &Node) -> &CharSet {
        match n {
            Node::Set(s) => s,
            other => panic!("not a set: {other:?}"),
        }
    }

    fn seq(n: &Node) -> &[Node] {
        match n {
            Node::Concat(v) => v,
            other => panic!("not a sequence: {other:?}"),
        }
    }

    #[test]
    fn literals_and_escapes_become_sets() {
        let p = parse_str("a\\x41\\u0042\\n\\cJ", "");
        let s = seq(&p.node);
        let singles: Vec<u32> = s
            .iter()
            .map(|n| set_of(n).single_member().unwrap())
            .collect();
        assert_eq!(singles, [0x61, 0x41, 0x42, 0x0A, 0x0A]);
        // Case closure under i.
        let p = parse_str("k", "i");
        assert_eq!(set_of(&p.node).ranges(), &[(0x4B, 0x4B), (0x6B, 0x6B)]);
        let p = parse_str("k", "iu");
        assert_eq!(
            set_of(&p.node).ranges(),
            &[(0x4B, 0x4B), (0x6B, 0x6B), (0x212A, 0x212A)]
        );
    }

    #[test]
    fn annex_b_forms_read_as_annex_b_says() {
        // `\u{2}` outside unicode mode is `u` twice (TASK-106).
        let p = parse_str("\\u{2}", "");
        match &p.node {
            Node::Repeat {
                body,
                min: 2,
                max: Some(2),
                ..
            } => assert_eq!(set_of(body).single_member(), Some(0x75)),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            set_of(&parse_str("\\u{2}", "u").node).single_member(),
            Some(2)
        );
        // An escaped surrogate pair is two code units outside unicode mode
        // (TASK-105), one code point in it.
        let p = parse_str("\\uD83D\\uDE00", "");
        let s = seq(&p.node);
        assert_eq!(set_of(&s[0]).single_member(), Some(0xD83D));
        assert_eq!(set_of(&s[1]).single_member(), Some(0xDE00));
        assert_eq!(
            set_of(&parse_str("\\uD83D\\uDE00", "u").node).single_member(),
            Some(0x1F600)
        );
        // Octal escapes, `\8`, a backreference past the group count.
        assert_eq!(
            set_of(&parse_str("\\101", "").node).single_member(),
            Some(0x41)
        );
        assert_eq!(
            set_of(&parse_str("\\8", "").node).single_member(),
            Some(0x38)
        );
        let p = parse_str("(a)\\2", "");
        assert_eq!(set_of(&seq(&p.node)[1]).single_member(), Some(2));
        assert!(matches!(
            seq(&parse_str("(a)\\1", "").node)[1],
            Node::BackRef { .. }
        ));
        // `\c` without a letter is a backslash, then `c`.
        let s = parse_str("\\c1", "");
        let singles: Vec<u32> = seq(&s.node)
            .iter()
            .map(|n| set_of(n).single_member().unwrap())
            .collect();
        assert_eq!(singles, [0x5C, 0x63, 0x31]);
        assert_eq!(
            set_of(&parse_str("[\\c1]", "").node).single_member(),
            Some(0x11)
        );
        // `[\d-z]` is \d, `-` and `z`.
        let s = set_of(&parse_str("[\\d-z]", "").node).clone();
        assert!(
            s.contains('5' as u32)
                && s.contains('-' as u32)
                && s.contains('z' as u32)
                && !s.contains('y' as u32)
        );
        // A lookahead can be quantified.
        assert!(matches!(parse_str("(?=a)*", "").node, Node::Repeat { .. }));
    }

    #[test]
    fn named_groups_resolve_to_numbers() {
        let p = parse_str("(?:(?<x>a)|(?<x>b))\\k<x>(?<y>c)", "");
        assert_eq!(
            p.names,
            [Some("x".into()), Some("x".into()), Some("y".into())]
        );
        match &seq(&p.node)[1] {
            Node::BackRef { groups, .. } => assert_eq!(groups, &[1, 2]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn negated_classes_fold_in_the_order_of_the_mode() {
        // Outside v mode `\P{Ll}` is closed after complementing, so under
        // ui it matches `a` (through `A`); v mode closes first.
        let u = set_of(&parse_str("\\P{Ll}", "ui").node).clone();
        assert!(u.contains('a' as u32));
        let v = set_of(&parse_str("\\P{Ll}", "vi").node).clone();
        assert!(!v.contains('a' as u32) && !v.contains('A' as u32));
        let n = set_of(&parse_str("[^a]", "i").node).clone();
        assert!(!n.contains('A' as u32) && n.contains('b' as u32));
        // \W under ui excludes the characters that fold into \w.
        let w = set_of(&parse_str("\\W", "ui").node).clone();
        assert!(!w.contains(0x17F) && !w.contains('S' as u32));
    }

    #[test]
    fn v_mode_classes_with_strings_try_the_longest_string_first() {
        let p = parse_str("[[\\q{abc|d|}a]--\\q{d}]", "v");
        match &p.node {
            Node::Alt(alts) => {
                assert_eq!(alts.len(), 3);
                assert_eq!(seq(&alts[0]).len(), 3);
                assert_eq!(set_of(&alts[1]).ranges(), &[(0x61, 0x61)]);
                assert!(matches!(alts[2], Node::Empty));
            }
            other => panic!("{other:?}"),
        }
        let p = parse_str("[[a-z]&&[aeiou]]", "v");
        assert_eq!(set_of(&p.node).len(), 5);
        assert!(matches!(
            parse_str("\\p{RGI_Emoji}", "v").node,
            Node::Alt(_)
        ));
    }

    #[test]
    fn word_boundaries_under_vi_use_the_folded_word_characters() {
        // TASK-107: regress left them out under vi.
        assert!(matches!(
            parse_str("\\b", "vi").node,
            Node::Assert(Assertion::Word {
                negate: false,
                extended: true
            })
        ));
        assert!(matches!(
            parse_str("\\B", "i").node,
            Node::Assert(Assertion::Word {
                negate: true,
                extended: false
            })
        ));
        assert!(matches!(
            parse_str("(?i:\\b)", "u").node,
            Node::Assert(Assertion::Word { extended: true, .. })
        ));
    }
}
