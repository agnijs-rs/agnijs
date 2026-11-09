//! Syntax validation for regular expression literals.
//!
//! Implements the ECMAScript `Pattern` grammar (ES2025 plus Annex B.1.2)
//! as a recogniser: `validate` returns `Ok(())` when `pattern` is a valid
//! pattern under `flags`, and a message otherwise. The parser checks
//! early errors (group counts, duplicate group names, out-of-order class
//! ranges, quantifier bounds, invalid escapes, `v`-mode class set rules).
//!
//! Unicode property names and values are checked against the engine's own
//! Unicode tables (`unicode.rs`).

use std::collections::HashSet;

/// Validate a regular expression literal. `pattern` is the literal's body
/// (between the slashes), `flags` its flag string.
pub fn validate(pattern: &str, flags: &str) -> Result<(), String> {
    let units: Vec<u16> = pattern.encode_utf16().collect();
    validate_literal(&units, flags)
}

/// [`validate`] for a literal whose body is UTF-16 code units (source text
/// from a JavaScript string can put lone surrogates in it).
pub fn validate_literal(pattern: &[u16], flags: &str) -> Result<(), String> {
    let mut seen = [false; 128];
    let mut unicode = false;
    let mut sets = false;
    for c in flags.chars() {
        if !matches!(c, 'd' | 'g' | 'i' | 'm' | 's' | 'u' | 'v' | 'y') {
            return Err(format!("invalid regular expression flag '{c}'"));
        }
        if seen[c as usize] {
            return Err(format!("duplicate regular expression flag '{c}'"));
        }
        seen[c as usize] = true;
        if c == 'u' {
            unicode = true;
        }
        if c == 'v' {
            sets = true;
        }
    }
    if unicode && sets {
        return Err("regular expression flags 'u' and 'v' are mutually exclusive".into());
    }
    validate_units(pattern, unicode, sets)
}

/// [`validate`] for a pattern given as UTF-16 code units (so lone
/// surrogates, which no `str` can hold, are checked too). `unicode` and
/// `sets` are the already-validated `u` and `v` flags.
pub fn validate_units(pattern: &[u16], unicode: bool, sets: bool) -> Result<(), String> {
    let unicode_mode = unicode || sets;
    // In unicode mode the pattern is a sequence of code points; otherwise
    // a sequence of UTF-16 code units.
    let units: Vec<u32> = if unicode_mode {
        let mut out = Vec::with_capacity(pattern.len());
        let mut i = 0;
        while i < pattern.len() {
            let c = pattern[i] as u32;
            if (0xD800..0xDC00).contains(&c) && i + 1 < pattern.len() {
                let l = pattern[i + 1] as u32;
                if (0xDC00..0xE000).contains(&l) {
                    out.push(0x10000 + ((c - 0xD800) << 10) + (l - 0xDC00));
                    i += 2;
                    continue;
                }
            }
            out.push(c);
            i += 1;
        }
        out
    } else {
        pattern.iter().map(|&u| u as u32).collect()
    };
    let (ngroups, names) = prescan(&units, sets);
    let mut p = P {
        s: units,
        i: 0,
        u: unicode_mode,
        v: sets,
        named: unicode_mode || !names.is_empty(),
        ngroups,
        names,
    };
    let visible = HashSet::new();
    p.disjunction(&visible)?;
    if p.i < p.s.len() {
        return Err(if p.s[p.i] == ')' as u32 {
            "unmatched ')'".into()
        } else {
            "unexpected character in regular expression".into()
        });
    }
    Ok(())
}

/// Count capturing groups and collect group names, so that backreferences
/// and `\k<name>` can be checked wherever they appear.
fn prescan(s: &[u32], sets: bool) -> (usize, HashSet<String>) {
    let mut count = 0;
    let mut names = HashSet::new();
    let mut i = 0;
    let mut depth = 0usize; // class nesting (v mode only nests)
    while i < s.len() {
        let c = s[i];
        if c == '\\' as u32 {
            i += 2;
            continue;
        }
        if c == '[' as u32 {
            if depth == 0 || sets {
                depth += 1;
            }
        } else if c == ']' as u32 {
            depth = depth.saturating_sub(1);
        } else if c == '(' as u32 && depth == 0 {
            if s.get(i + 1) != Some(&('?' as u32)) {
                count += 1;
            } else if s.get(i + 2) == Some(&('<' as u32))
                && !matches!(s.get(i + 3), Some(&x) if x == '=' as u32 || x == '!' as u32)
            {
                count += 1;
                // Collect the raw name text (escapes resolved later; a raw
                // comparison is enough to decide whether `\k` is legal).
                let mut j = i + 3;
                let mut name = String::new();
                while j < s.len() && s[j] != '>' as u32 {
                    name.push(char::from_u32(s[j]).unwrap_or('\u{FFFD}'));
                    j += 1;
                }
                names.insert(name);
            }
        }
        i += 1;
    }
    (count, names)
}

struct P {
    s: Vec<u32>,
    i: usize,
    /// Unicode mode (`u` or `v`).
    u: bool,
    /// UnicodeSets mode (`v`).
    v: bool,
    /// The `NamedCaptureGroups` grammar parameter.
    named: bool,
    ngroups: usize,
    /// Raw group names as written (without escape decoding); decoded names
    /// are added as groups are parsed.
    names: HashSet<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClassAtom {
    Char(u32),
    /// A character class escape such as `\d` (no single code point).
    Set,
}

fn u(c: char) -> u32 {
    c as u32
}

impl P {
    fn peek(&self) -> Option<u32> {
        self.s.get(self.i).copied()
    }
    fn peek_at(&self, n: usize) -> Option<u32> {
        self.s.get(self.i + n).copied()
    }
    fn at(&self, c: char) -> bool {
        self.peek() == Some(u(c))
    }
    fn at_str(&self, t: &str) -> bool {
        t.chars()
            .enumerate()
            .all(|(k, c)| self.peek_at(k) == Some(u(c)))
    }
    fn eat(&mut self, c: char) -> bool {
        if self.at(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn err<T>(&self, m: impl Into<String>) -> Result<T, String> {
        Err(m.into())
    }

    // ---- disjunction / alternative / term ---------------------------------

    /// Returns the group names declared inside.
    fn disjunction(&mut self, visible: &HashSet<String>) -> Result<HashSet<String>, String> {
        let mut declared = HashSet::new();
        loop {
            let names = self.alternative(visible)?;
            declared.extend(names);
            if !self.eat('|') {
                break;
            }
        }
        Ok(declared)
    }

    fn alternative(&mut self, visible: &HashSet<String>) -> Result<HashSet<String>, String> {
        let mut visible = visible.clone();
        let mut declared = HashSet::new();
        while let Some(c) = self.peek() {
            if c == u('|') || c == u(')') {
                break;
            }
            self.term(&mut visible, &mut declared)?;
        }
        Ok(declared)
    }

    fn term(
        &mut self,
        visible: &mut HashSet<String>,
        declared: &mut HashSet<String>,
    ) -> Result<(), String> {
        let c = self.peek().unwrap();
        // Assertions.
        if c == u('^') || c == u('$') {
            self.i += 1;
            return Ok(());
        }
        if c == u('\\') && matches!(self.peek_at(1), Some(x) if x == u('b') || x == u('B')) {
            self.i += 2;
            return Ok(());
        }
        if c == u('(') && self.peek_at(1) == Some(u('?')) {
            let lookahead = self.at_str("(?=") || self.at_str("(?!");
            let lookbehind = self.at_str("(?<=") || self.at_str("(?<!");
            if lookahead || lookbehind {
                self.i += if lookahead { 3 } else { 4 };
                let names = self.disjunction(visible)?;
                if !self.eat(')') {
                    return self.err("unterminated group");
                }
                self.declare_all(names, visible, declared);
                // Annex B: lookaheads may be quantified in non-unicode mode.
                if lookahead && !self.u {
                    self.quantifier_opt()?;
                } else if self.at_quantifier_start() {
                    return self.err("nothing to repeat");
                }
                return Ok(());
            }
        }
        self.atom(visible, declared)?;
        self.quantifier_opt()
    }

    fn declare_all(
        &mut self,
        names: HashSet<String>,
        visible: &mut HashSet<String>,
        declared: &mut HashSet<String>,
    ) {
        for n in names {
            visible.insert(n.clone());
            declared.insert(n);
        }
    }

    fn at_quantifier_start(&self) -> bool {
        match self.peek() {
            Some(c) if c == u('*') || c == u('+') || c == u('?') => true,
            Some(c) if c == u('{') => self.braced_quantifier_len().is_some(),
            _ => false,
        }
    }

    /// If a full `{n}`, `{n,}` or `{n,m}` starts at `i`, return its length
    /// and bounds.
    fn braced_quantifier(&self) -> Option<(usize, u128, Option<u128>)> {
        let mut j = self.i + 1;
        let digits = |j: &mut usize| -> Option<u128> {
            let start = *j;
            let mut v: u128 = 0;
            while let Some(&d) = self.s.get(*j) {
                if (u('0')..=u('9')).contains(&d) {
                    v = v.saturating_mul(10).saturating_add((d - u('0')) as u128);
                    *j += 1;
                } else {
                    break;
                }
            }
            if *j == start {
                None
            } else {
                Some(v)
            }
        };
        let min = digits(&mut j)?;
        let mut max = Some(min);
        if self.s.get(j) == Some(&u(',')) {
            j += 1;
            max = digits(&mut j);
            if max.is_none() && self.s.get(j) != Some(&u('}')) {
                return None;
            }
        }
        if self.s.get(j) != Some(&u('}')) {
            return None;
        }
        Some((j + 1 - self.i, min, max))
    }

    fn braced_quantifier_len(&self) -> Option<usize> {
        self.braced_quantifier().map(|q| q.0)
    }

    fn quantifier_opt(&mut self) -> Result<(), String> {
        match self.peek() {
            Some(c) if c == u('*') || c == u('+') || c == u('?') => {
                self.i += 1;
            }
            Some(c) if c == u('{') => match self.braced_quantifier() {
                Some((len, min, max)) => {
                    if let Some(max) = max {
                        if max < min {
                            return self.err("numbers out of order in {} quantifier");
                        }
                    }
                    self.i += len;
                }
                None => {
                    if self.u {
                        return self.err("incomplete quantifier");
                    }
                    return Ok(());
                }
            },
            _ => return Ok(()),
        }
        self.eat('?');
        if self.at_quantifier_start() {
            return self.err("nothing to repeat");
        }
        Ok(())
    }

    // ---- atoms -------------------------------------------------------------

    fn atom(
        &mut self,
        visible: &mut HashSet<String>,
        declared: &mut HashSet<String>,
    ) -> Result<(), String> {
        let c = self.peek().unwrap();
        match char::from_u32(c) {
            Some('.') => {
                self.i += 1;
                Ok(())
            }
            Some('(') => self.group(visible, declared),
            Some(')') => self.err("unmatched ')'"),
            Some('[') => self.class(),
            Some('\\') => self.atom_escape(),
            Some('*') | Some('+') | Some('?') => self.err("nothing to repeat"),
            Some('{') => {
                if self.u {
                    return self.err("lone quantifier bracket");
                }
                // Annex B: a complete braced quantifier here is an error,
                // anything else makes `{` a literal.
                if self.braced_quantifier_len().is_some() {
                    return self.err("nothing to repeat");
                }
                self.i += 1;
                Ok(())
            }
            Some('}') | Some(']') => {
                if self.u {
                    return self.err("lone quantifier bracket");
                }
                self.i += 1;
                Ok(())
            }
            _ => {
                self.i += 1;
                Ok(())
            }
        }
    }

    fn group(
        &mut self,
        visible: &mut HashSet<String>,
        declared: &mut HashSet<String>,
    ) -> Result<(), String> {
        self.i += 1; // (
        if self.eat('?') {
            if self.eat(':') {
                let names = self.disjunction(visible)?;
                self.declare_all(names, visible, declared);
            } else if self.at('<') {
                self.i += 1;
                let name = self.group_name()?;
                if visible.contains(&name) {
                    return self.err(format!("duplicate capture group name '{name}'"));
                }
                self.names.insert(name.clone());
                visible.insert(name.clone());
                declared.insert(name);
                let names = self.disjunction(visible)?;
                self.declare_all(names, visible, declared);
            } else {
                // Modifiers: (?ims-ims: ... )
                let mut add = String::new();
                let mut remove = String::new();
                let mut seen_dash = false;
                loop {
                    match self.peek().and_then(char::from_u32) {
                        Some(ch @ ('i' | 'm' | 's')) => {
                            if add.contains(ch) || remove.contains(ch) {
                                return self.err("repeated regular expression modifier");
                            }
                            if seen_dash {
                                remove.push(ch)
                            } else {
                                add.push(ch)
                            }
                            self.i += 1;
                        }
                        Some('-') if !seen_dash => {
                            seen_dash = true;
                            self.i += 1;
                        }
                        Some(':') => {
                            self.i += 1;
                            break;
                        }
                        _ => return self.err("invalid group"),
                    }
                }
                if add.is_empty() && remove.is_empty() {
                    return self.err("invalid regular expression modifiers");
                }
                let names = self.disjunction(visible)?;
                self.declare_all(names, visible, declared);
            }
        } else {
            let names = self.disjunction(visible)?;
            self.declare_all(names, visible, declared);
        }
        if !self.eat(')') {
            return self.err("unterminated group");
        }
        Ok(())
    }

    /// Parses `Name>` after `<` and returns the decoded name.
    fn group_name(&mut self) -> Result<String, String> {
        let mut name = String::new();
        let mut first = true;
        loop {
            let Some(c) = self.peek() else {
                return self.err("invalid capture group name");
            };
            if c == u('>') {
                self.i += 1;
                break;
            }
            let cp = if c == u('\\') {
                self.i += 1;
                if !self.eat('u') {
                    return self.err("invalid capture group name");
                }
                // RegExpUnicodeEscapeSequence with [+UnicodeMode] always.
                self.unicode_escape_body(true)?
            } else {
                self.i += 1;
                let mut cp = c;
                if (0xD800..0xDC00).contains(&c) {
                    if let Some(t) = self.peek() {
                        if (0xDC00..0xE000).contains(&t) {
                            self.i += 1;
                            cp = 0x10000 + ((c - 0xD800) << 10) + (t - 0xDC00);
                        }
                    }
                }
                cp
            };
            let ch = char::from_u32(cp).ok_or_else(|| "invalid capture group name".to_string())?;
            let ok = if first {
                ch == '$' || ch == '_' || unicode_id_start::is_id_start(ch)
            } else {
                ch == '$'
                    || ch == '\u{200C}'
                    || ch == '\u{200D}'
                    || unicode_id_start::is_id_continue(ch)
            };
            if !ok {
                return self.err("invalid capture group name");
            }
            name.push(ch);
            first = false;
        }
        if name.is_empty() {
            return self.err("invalid capture group name");
        }
        Ok(name)
    }

    /// After `\u`: parses `XXXX`, `{X...}` (unicode mode) and surrogate
    /// pairs `XXXX\uXXXX` (unicode mode). Returns the code point.
    fn unicode_escape_body(&mut self, unicode: bool) -> Result<u32, String> {
        if unicode && self.eat('{') {
            let mut v: u32 = 0;
            let mut any = false;
            while let Some(d) = self
                .peek()
                .and_then(char::from_u32)
                .and_then(|c| c.to_digit(16))
            {
                v = v.saturating_mul(16).saturating_add(d);
                if v > 0x10FFFF {
                    return self.err("invalid unicode escape");
                }
                any = true;
                self.i += 1;
            }
            if !any || !self.eat('}') {
                return self.err("invalid unicode escape");
            }
            return Ok(v);
        }
        let lead = self
            .hex4()
            .ok_or_else(|| "invalid unicode escape".to_string())?;
        if unicode && (0xD800..0xDC00).contains(&lead) && self.at_str("\\u") {
            let save = self.i;
            self.i += 2;
            match self.hex4() {
                Some(t) if (0xDC00..0xE000).contains(&t) => {
                    return Ok(0x10000 + ((lead - 0xD800) << 10) + (t - 0xDC00));
                }
                _ => self.i = save,
            }
        }
        Ok(lead)
    }

    fn hex4(&mut self) -> Option<u32> {
        let mut v = 0;
        for k in 0..4 {
            let d = self
                .peek_at(k)
                .and_then(char::from_u32)
                .and_then(|c| c.to_digit(16))?;
            v = v * 16 + d;
        }
        self.i += 4;
        Some(v)
    }

    fn hex2(&mut self) -> Option<u32> {
        let mut v = 0;
        for k in 0..2 {
            let d = self
                .peek_at(k)
                .and_then(char::from_u32)
                .and_then(|c| c.to_digit(16))?;
            v = v * 16 + d;
        }
        self.i += 2;
        Some(v)
    }

    // ---- escapes -----------------------------------------------------------

    fn atom_escape(&mut self) -> Result<(), String> {
        self.i += 1; // backslash
        let Some(c) = self.peek() else {
            return self.err("\\ at end of pattern");
        };
        let ch = char::from_u32(c);
        match ch {
            // Backreference or legacy octal.
            Some('1'..='9') => {
                let start = self.i;
                let mut n: usize = 0;
                while let Some(d) = self
                    .peek()
                    .and_then(char::from_u32)
                    .and_then(|c| c.to_digit(10))
                {
                    n = n.saturating_mul(10).saturating_add(d as usize);
                    self.i += 1;
                }
                if n <= self.ngroups {
                    return Ok(());
                }
                if self.u {
                    return self.err("invalid backreference");
                }
                // Annex B: reparse as legacy octal escape / identity escape.
                self.i = start;
                self.legacy_octal_or_identity();
                Ok(())
            }
            Some('0') => {
                self.i += 1;
                if matches!(self.peek().and_then(char::from_u32), Some('0'..='9')) {
                    if self.u {
                        return self.err("invalid decimal escape");
                    }
                    // Legacy octal: up to two more octal digits.
                    self.i -= 1;
                    self.legacy_octal_or_identity();
                }
                Ok(())
            }
            Some('k') => {
                self.i += 1;
                if self.named {
                    if !self.eat('<') {
                        return self.err("invalid named reference");
                    }
                    let name = self.group_name()?;
                    if !self.names.contains(&name) {
                        return self.err(format!("invalid named reference '{name}'"));
                    }
                    Ok(())
                } else {
                    Ok(()) // identity escape
                }
            }
            Some('d') | Some('D') | Some('s') | Some('S') | Some('w') | Some('W') => {
                self.i += 1;
                Ok(())
            }
            Some('p') | Some('P') => {
                if !self.u {
                    self.i += 1;
                    return Ok(()); // identity escape
                }
                let negated = ch == Some('P');
                self.i += 1;
                let strings = self.property_escape()?;
                if strings && negated {
                    return self.err("negated property of strings");
                }
                Ok(())
            }
            _ => {
                self.character_escape(false)?;
                Ok(())
            }
        }
    }

    /// Annex B `\` DecimalDigits in non-unicode mode: `\0`-`\377` octal, or
    /// `\8`/`\9` identity.
    fn legacy_octal_or_identity(&mut self) {
        let d0 = self.peek().unwrap();
        if d0 == u('8') || d0 == u('9') {
            self.i += 1;
            return;
        }
        let is_oct = |c: Option<u32>| matches!(c, Some(x) if (u('0')..=u('7')).contains(&x));
        self.i += 1;
        if is_oct(self.peek()) {
            let two = (d0 - u('0')) * 8 + (self.peek().unwrap() - u('0'));
            self.i += 1;
            if two <= 0o37 && is_oct(self.peek()) {
                self.i += 1;
            }
        }
    }

    /// After `\p`/`\P`: parses `{Name}` or `{Name=Value}`. Returns whether
    /// the property is a property of strings (only legal in `v` mode).
    fn property_escape(&mut self) -> Result<bool, String> {
        if !self.eat('{') {
            return self.err("invalid property name");
        }
        let mut body = String::new();
        loop {
            match self.peek().and_then(char::from_u32) {
                Some('}') => {
                    self.i += 1;
                    break;
                }
                Some(c) if c.is_ascii_alphanumeric() || c == '_' || c == '=' => {
                    body.push(c);
                    self.i += 1;
                }
                _ => return self.err("invalid property name"),
            }
        }
        if crate::unicode::is_string_property(&body) {
            if !self.v {
                return self.err("invalid property name");
            }
            return Ok(true);
        }
        if body.is_empty()
            || body.starts_with('=')
            || body.ends_with('=')
            || body.matches('=').count() > 1
        {
            return self.err("invalid property name");
        }
        let known = match body.split_once('=') {
            Some((name, value)) => crate::unicode::property(name, Some(value)).is_some(),
            None => crate::unicode::property(&body, None).is_some(),
        };
        if !known {
            return self.err(format!("invalid property name '{body}'"));
        }
        Ok(false)
    }

    /// CharacterEscape (after the backslash, at the escape character).
    /// Returns the code point of the escaped character, or `None` when it
    /// is an identity escape of a character handled by the caller.
    /// `in_class` enables the Annex B `\c` digit/underscore form.
    fn character_escape(&mut self, in_class: bool) -> Result<u32, String> {
        let c = self.peek().unwrap();
        let ch = char::from_u32(c);
        match ch {
            Some('f') => {
                self.i += 1;
                Ok(0x0C)
            }
            Some('n') => {
                self.i += 1;
                Ok(0x0A)
            }
            Some('r') => {
                self.i += 1;
                Ok(0x0D)
            }
            Some('t') => {
                self.i += 1;
                Ok(0x09)
            }
            Some('v') => {
                self.i += 1;
                Ok(0x0B)
            }
            Some('c') => {
                let next = self.peek_at(1).and_then(char::from_u32);
                match next {
                    Some(l) if l.is_ascii_alphabetic() => {
                        self.i += 2;
                        Ok((l as u32) % 32)
                    }
                    Some(l) if in_class && !self.u && (l.is_ascii_digit() || l == '_') => {
                        self.i += 2;
                        Ok((l as u32) % 32)
                    }
                    _ => {
                        if self.u {
                            return self.err("invalid unicode escape");
                        }
                        // Annex B: the backslash is a literal `\`; leave `c`
                        // to be read as the next atom.
                        Ok(u('\\'))
                    }
                }
            }
            Some('x') => {
                self.i += 1;
                match self.hex2() {
                    Some(v) => Ok(v),
                    None => {
                        if self.u {
                            return self.err("invalid escape");
                        }
                        Ok(u('x'))
                    }
                }
            }
            Some('u') => {
                self.i += 1;
                let save = self.i;
                match self.unicode_escape_body(self.u) {
                    Ok(v) => Ok(v),
                    Err(e) => {
                        if self.u {
                            return Err(e);
                        }
                        self.i = save;
                        Ok(u('u'))
                    }
                }
            }
            Some('0') => {
                // Only reached from class escapes in non-unicode mode or as
                // `\0` not followed by a digit.
                self.i += 1;
                if matches!(self.peek().and_then(char::from_u32), Some('0'..='9')) {
                    if self.u {
                        return self.err("invalid class escape");
                    }
                    self.i -= 1;
                    self.legacy_octal_or_identity();
                }
                Ok(0)
            }
            Some('1'..='9') if !self.u => {
                self.legacy_octal_or_identity();
                Ok(0)
            }
            _ => {
                // IdentityEscape.
                if self.u {
                    let ok = ch.map_or(false, |c| "^$\\.*+?()[]{}|/".contains(c));
                    if !ok {
                        return self.err("invalid escape");
                    }
                    self.i += 1;
                    return Ok(c);
                }
                if ch == Some('k') && self.named {
                    return self.err("invalid named reference");
                }
                self.i += 1;
                Ok(c)
            }
        }
    }

    // ---- character classes -------------------------------------------------

    fn class(&mut self) -> Result<(), String> {
        self.i += 1; // [
        if self.v {
            let negated = self.eat('^');
            let may_contain_strings = self.class_set_expression()?;
            if negated && may_contain_strings {
                return self.err("negated character class may contain strings");
            }
            if !self.eat(']') {
                return self.err("unterminated character class");
            }
            return Ok(());
        }
        self.eat('^');
        loop {
            if self.at(']') {
                self.i += 1;
                return Ok(());
            }
            if self.peek().is_none() {
                return self.err("unterminated character class");
            }
            let a = self.class_atom()?;
            if self.at('-') && self.peek_at(1).is_some() && self.peek_at(1) != Some(u(']')) {
                self.i += 1; // -
                let b = self.class_atom()?;
                match (a, b) {
                    (ClassAtom::Char(x), ClassAtom::Char(y)) => {
                        if x > y {
                            return self.err("range out of order in character class");
                        }
                    }
                    _ => {
                        if self.u {
                            return self.err("invalid character class range");
                        }
                        // Annex B: `-` is literal.
                    }
                }
            }
        }
    }

    fn class_atom(&mut self) -> Result<ClassAtom, String> {
        let c = self.peek().unwrap();
        if c != u('\\') {
            self.i += 1;
            return Ok(ClassAtom::Char(c));
        }
        self.i += 1;
        let Some(e) = self.peek() else {
            return self.err("\\ at end of pattern");
        };
        match char::from_u32(e) {
            Some('b') => {
                self.i += 1;
                Ok(ClassAtom::Char(8))
            }
            Some('-') if self.u => {
                self.i += 1;
                Ok(ClassAtom::Char(u('-')))
            }
            Some('d') | Some('D') | Some('s') | Some('S') | Some('w') | Some('W') => {
                self.i += 1;
                Ok(ClassAtom::Set)
            }
            Some('p') | Some('P') if self.u => {
                self.i += 1;
                let strings = self.property_escape()?;
                if strings {
                    return self.err("property of strings in character class");
                }
                Ok(ClassAtom::Set)
            }
            Some('k') if self.u => self.err("invalid escape"),
            Some('1'..='9') if self.u => self.err("invalid class escape"),
            _ => Ok(ClassAtom::Char(self.character_escape(true)?)),
        }
    }

    // ---- v-mode class sets -------------------------------------------------

    /// ClassSetExpression (may be empty). Returns MayContainStrings.
    fn class_set_expression(&mut self) -> Result<bool, String> {
        if self.at(']') {
            return Ok(false);
        }
        // First operand decides between union / intersection / subtraction.
        let (first_strings, first_char) = self.class_set_operand_or_range()?;
        if self.at_str("&&") {
            let mut strings = first_strings;
            while self.at_str("&&") {
                self.i += 2;
                if self.at('&') {
                    return self.err("invalid set operation in character class");
                }
                let (s, _) = self.class_set_operand()?;
                strings = strings && s;
            }
            if !self.at(']') {
                return self.err("invalid set operation in character class");
            }
            return Ok(strings);
        }
        if self.at_str("--") {
            while self.at_str("--") {
                self.i += 2;
                self.class_set_operand()?;
            }
            if !self.at(']') {
                return self.err("invalid set operation in character class");
            }
            return Ok(first_strings);
        }
        let _ = first_char;
        let mut strings = first_strings;
        while !self.at(']') {
            if self.peek().is_none() {
                return self.err("unterminated character class");
            }
            if self.at_str("&&") || self.at_str("--") {
                return self.err("invalid set operation in character class");
            }
            let (s, _) = self.class_set_operand_or_range()?;
            strings = strings || s;
        }
        Ok(strings)
    }

    /// A ClassSetOperand, or a ClassSetRange when a `-` follows a character.
    fn class_set_operand_or_range(&mut self) -> Result<(bool, Option<u32>), String> {
        let (strings, ch) = self.class_set_operand()?;
        if let Some(lo) = ch {
            if self.at('-') && !self.at_str("--") {
                self.i += 1;
                let hi = self.class_set_character()?;
                if lo > hi {
                    return self.err("range out of order in character class");
                }
                return Ok((false, None));
            }
        }
        Ok((strings, ch))
    }

    /// Returns (MayContainStrings, Some(code point) when a single character).
    fn class_set_operand(&mut self) -> Result<(bool, Option<u32>), String> {
        let Some(c) = self.peek() else {
            return self.err("unterminated character class");
        };
        if c == u('[') {
            self.i += 1;
            let negated = self.eat('^');
            let strings = self.class_set_expression()?;
            if negated && strings {
                return self.err("negated character class may contain strings");
            }
            if !self.eat(']') {
                return self.err("unterminated character class");
            }
            return Ok((strings, None));
        }
        if c == u('\\') {
            match self.peek_at(1).and_then(char::from_u32) {
                Some('q') => {
                    self.i += 2;
                    if !self.eat('{') {
                        return self.err("invalid escape in character class");
                    }
                    let mut strings = false;
                    loop {
                        let mut len = 0;
                        while !self.at('|') && !self.at('}') {
                            if self.peek().is_none() {
                                return self.err("unterminated class string disjunction");
                            }
                            self.class_set_character()?;
                            len += 1;
                        }
                        if len != 1 {
                            strings = true;
                        }
                        if self.eat('}') {
                            break;
                        }
                        self.i += 1; // |
                    }
                    return Ok((strings, None));
                }
                Some('d' | 'D' | 's' | 'S' | 'w' | 'W') => {
                    self.i += 2;
                    return Ok((false, None));
                }
                Some(p @ ('p' | 'P')) => {
                    self.i += 2;
                    let strings = self.property_escape()?;
                    if strings && p == 'P' {
                        return self.err("negated property of strings");
                    }
                    return Ok((strings, None));
                }
                _ => {}
            }
        }
        let cp = self.class_set_character()?;
        Ok((false, Some(cp)))
    }

    fn class_set_character(&mut self) -> Result<u32, String> {
        let Some(c) = self.peek() else {
            return self.err("unterminated character class");
        };
        let ch = char::from_u32(c);
        if c == u('\\') {
            let Some(e) = self.peek_at(1).and_then(char::from_u32) else {
                return self.err("\\ at end of pattern");
            };
            if "&-!#%,:;<=>@`~".contains(e) {
                self.i += 2;
                return Ok(e as u32);
            }
            if e == 'b' {
                self.i += 2;
                return Ok(8);
            }
            self.i += 1;
            return self.character_escape(false);
        }
        if let Some(ch) = ch {
            if "()[]{}/-\\|".contains(ch) {
                return self.err("invalid character in character class");
            }
            if let Some(next) = self.peek_at(1).and_then(char::from_u32) {
                if next == ch && "&!#$%*+,.:;<=>?@^`~".contains(ch) {
                    return self.err("invalid set operation in character class");
                }
            }
        }
        self.i += 1;
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::validate;

    fn ok(p: &str, f: &str) {
        if let Err(e) = validate(p, f) {
            panic!("/{p}/{f} should be valid: {e}");
        }
    }
    fn bad(p: &str, f: &str) {
        if validate(p, f).is_ok() {
            panic!("/{p}/{f} should be invalid");
        }
    }

    #[test]
    fn flags() {
        ok("a", "dgimsuy");
        bad("a", "uv");
        bad("a", "gg");
        bad("a", "x");
    }

    #[test]
    fn basics() {
        ok("a|b*c+d?e{2}f{2,}g{2,3}", "");
        ok("(a)(?:b)(?<n>c)\\1\\k<n>", "");
        bad("(", "");
        bad(")", "");
        bad("*a", "");
        bad("a**", "");
        bad("a{3,2}", "");
        ok("a{,2}", "");
        bad("a{,2}", "u");
        ok("a{", "");
        bad("{2}", "");
        ok("]", "");
        bad("]", "u");
    }

    #[test]
    fn escapes() {
        ok("\\u{1F600}", "u");
        bad("\\u{110000}", "u");
        ok("\\u{1F600}", "");
        ok("\\x", "");
        bad("\\x", "u");
        ok("\\c", "");
        bad("\\c", "u");
        ok("\\a", "");
        bad("\\a", "u");
        ok("\\/", "u");
        ok("\\2", "");
        bad("\\2", "u");
        ok("\\0", "u");
        bad("\\00", "u");
        ok("\\00", "");
        ok("\\k", "");
        bad("\\k", "u");
        bad("(?<a>)\\k", "");
        bad("(?<a>)\\k<b>", "");
        ok("\\k<a>(?<a>)", "");
    }

    #[test]
    fn groups() {
        bad("(?<a>)(?<a>)", "");
        ok("(?<a>)|(?<a>)", "");
        ok("(?:(?<a>)|(?<a>))(?<b>)", "");
        bad("(?:(?<a>)|(?<a>))(?<a>)", "");
        ok("(?<$𝒜>)", "");
        ok("(?<\\u{1d49c}>)", "");
        bad("(?<1a>)", "");
        ok("(?=a)*", "");
        bad("(?=a)*", "u");
        bad("(?<=a)*", "");
        ok("(?i:a)(?-i:b)(?i-s:c)", "");
        bad("(?ii:a)", "");
        bad("(?-:a)", "");
        bad("(?i-i:a)", "");
    }

    #[test]
    fn classes() {
        ok("[a-z]", "");
        bad("[z-a]", "");
        ok("[\\d-z]", "");
        bad("[\\d-z]", "u");
        ok("[\\c1]", "");
        ok("[\\-]", "u");
        bad("[", "");
        ok("[]]", "");
        ok("[\\p{Script=Greek}\\p{Lu}\\p{Any}]", "u");
        bad("\\p{Bogus}", "u");
        bad("\\p{Script}", "u");
        ok("\\p{Script}", "");
    }

    #[test]
    fn unicode_sets() {
        ok("[\\p{L}--[a-z]]", "v");
        ok("[[a-z]&&[aeiou]]", "v");
        ok("[\\q{abc|d}]", "v");
        bad("[^\\q{abc}]", "v");
        ok("[^\\q{a}]", "v");
        bad("[a--b&&c]", "v");
        bad("[(]", "v");
        ok("[\\(]", "v");
        bad("[&&a]", "v");
        bad("[a&&&b]", "v");
        bad("[a!!b]", "v");
        ok("\\p{RGI_Emoji}", "v");
        bad("\\P{RGI_Emoji}", "v");
        bad("\\p{RGI_Emoji}", "u");
        ok("[]", "v");
        ok("[a-z--c]", "v");
    }
}
