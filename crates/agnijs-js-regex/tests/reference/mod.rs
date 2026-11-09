//! The previous matcher, `regress` behind the pre-TASK-100.31 seam
//! adapter, kept as a test-only reference for differential fuzzing.
//! Group names are resolved to numbers before regress sees the pattern
//! and sticky matching runs `(?:P)()|`, exactly as the old seam did.

#![allow(dead_code)]

use agnijs_js_regex::{Captures, Flags};

pub struct Reference {
    inner: regress::Regex,
    anchored: regress::Regex,
    groups: usize,
    unicode_mode: bool,
}

impl Reference {
    pub fn compile(pattern: &[u16], flags: &Flags) -> Option<Reference> {
        let unicode_mode = flags.unicode_mode();
        let rflags = regress::Flags {
            icase: flags.ignore_case,
            multiline: flags.multiline,
            dot_all: flags.dot_all,
            no_opt: false,
            unicode: flags.unicode,
            unicode_sets: flags.unicode_sets,
        };
        let code_points = pattern_code_points(pattern, unicode_mode);
        let names = group_names(&code_points, unicode_mode);
        let stripped = strip_group_names(&code_points, unicode_mode, &names);
        let inner = regress::Regex::from_unicode(stripped.iter().copied(), rflags).ok()?;
        let mut p: Vec<u32> = "(?:".chars().map(|c| c as u32).collect();
        p.extend(pin_group_count_escapes(&stripped, names.len(), unicode_mode));
        p.extend(")()|".chars().map(|c| c as u32));
        let anchored = regress::Regex::from_unicode(p.into_iter(), rflags).ok()?;
        Some(Reference { inner, anchored, groups: names.len(), unicode_mode })
    }

    fn find(&self, re: &regress::Regex, input: &[u16], start: usize) -> Option<regress::Match> {
        if start > input.len() {
            return None;
        }
        if self.unicode_mode {
            re.find_from_utf16(input, start).next()
        } else {
            re.find_from_ucs2(input, start).next()
        }
    }

    fn captures(&self, m: &regress::Match, groups: usize) -> Captures {
        let mut ranges = vec![Some((m.range.start, m.range.end))];
        for c in &m.captures[..groups] {
            ranges.push(c.as_ref().map(|r| (r.start, r.end)));
        }
        while ranges.len() < self.groups + 1 {
            ranges.push(None);
        }
        Captures { ranges }
    }

    pub fn exec(&self, input: &[u16], start: usize) -> Option<Captures> {
        let m = self.find(&self.inner, input, start)?;
        Some(self.captures(&m, m.captures.len()))
    }

    pub fn exec_sticky(&self, input: &[u16], start: usize) -> Option<Captures> {
        let m = self.find(&self.anchored, input, start)?;
        let groups = m.captures.len() - 1;
        if m.range.start != start || m.captures[groups].is_none() {
            return None;
        }
        Some(self.captures(&m, groups))
    }
}

/// The pattern as the code points the grammar sees: surrogate pairs join
/// in unicode mode, every code unit stands alone otherwise.
fn pattern_code_points(pattern: &[u16], unicode_mode: bool) -> Vec<u32> {
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

/// Capture-group names in group order, from a pattern the grammar has
/// already accepted. Escapes are skipped, character classes (including
/// nested `v`-mode classes) are skipped, `(?<=` and `(?<!` are lookbehinds,
/// `(?:` and `(?flags:` do not capture.
fn group_names(p: &[u32], unicode_sets: bool) -> Vec<Option<String>> {
    let mut names = Vec::new();
    let mut i = 0;
    let mut class_depth = 0usize;
    while i < p.len() {
        let c = p[i];
        if c == '\\' as u32 {
            i += 2;
            continue;
        }
        if class_depth > 0 {
            if c == ']' as u32 {
                class_depth -= 1;
            } else if c == '[' as u32 && unicode_sets {
                class_depth += 1;
            }
            i += 1;
            continue;
        }
        if c == '[' as u32 {
            class_depth = 1;
            i += 1;
            continue;
        }
        if c == '(' as u32 {
            if p.get(i + 1) == Some(&('?' as u32)) {
                if p.get(i + 2) == Some(&('<' as u32))
                    && !matches!(p.get(i + 3), Some(&x) if x == '=' as u32 || x == '!' as u32)
                {
                    let (name, next) = read_group_name(p, i + 3);
                    names.push(Some(name));
                    i = next;
                    continue;
                }
                // Non-capturing, lookaround or modifier group.
                i += 2;
                continue;
            }
            names.push(None);
        }
        i += 1;
    }
    names
}

/// Read a GroupName starting at `start` (just after `<`) up to and
/// including the closing `>`; returns the name and the index after `>`.
/// Escapes (`\u{...}`, `\uXXXX` with surrogate pairs) and raw surrogate
/// pairs (non-unicode mode keeps them as two units) are decoded.
fn read_group_name(p: &[u32], start: usize) -> (String, usize) {
    let mut j = start;
    let mut name = String::new();
    while j < p.len() && p[j] != '>' as u32 {
        let (cp, next) = if p[j] == '\\' as u32 {
            name_escape(p, j)
        } else if (0xD800..0xDC00).contains(&p[j])
            && matches!(p.get(j + 1), Some(&l) if (0xDC00..0xE000).contains(&l))
        {
            (0x10000 + ((p[j] - 0xD800) << 10) + (p[j + 1] - 0xDC00), j + 2)
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

/// Rewrite `(?<name>` as `(` and `\k<name>` as the numbered references
/// of every group with that name, wrapped in a non-capturing group so a
/// following digit cannot extend the number. Duplicate names live in
/// different alternatives, so at most one of them holds a capture at any
/// time and concatenating the references (`\1\2`) matches exactly the
/// participating group (unset groups match the empty string).
fn strip_group_names(p: &[u32], unicode_sets: bool, names: &[Option<String>]) -> Vec<u32> {
    if names.iter().all(|n| n.is_none()) {
        return p.to_vec();
    }
    let mut out = Vec::with_capacity(p.len());
    let mut i = 0;
    let mut class_depth = 0usize;
    while i < p.len() {
        let c = p[i];
        if c == '\\' as u32 {
            if class_depth == 0 && p.get(i + 1) == Some(&('k' as u32)) && p.get(i + 2) == Some(&('<' as u32)) {
                let (name, next) = read_group_name(p, i + 3);
                out.extend("(?:".chars().map(|c| c as u32));
                for (idx, n) in names.iter().enumerate() {
                    if n.as_deref() == Some(name.as_str()) {
                        out.push('\\' as u32);
                        out.extend((idx + 1).to_string().chars().map(|c| c as u32));
                    }
                }
                out.push(')' as u32);
                i = next;
                continue;
            }
            out.push(c);
            if let Some(&n) = p.get(i + 1) {
                out.push(n);
            }
            i += 2;
            continue;
        }
        if class_depth > 0 {
            if c == ']' as u32 {
                class_depth -= 1;
            } else if c == '[' as u32 && unicode_sets {
                class_depth += 1;
            }
            out.push(c);
            i += 1;
            continue;
        }
        if c == '[' as u32 {
            class_depth = 1;
            out.push(c);
            i += 1;
            continue;
        }
        if c == '(' as u32
            && p.get(i + 1) == Some(&('?' as u32))
            && p.get(i + 2) == Some(&('<' as u32))
            && !matches!(p.get(i + 3), Some(&x) if x == '=' as u32 || x == '!' as u32)
        {
            let (_, next) = read_group_name(p, i + 3);
            out.push('(' as u32);
            i = next;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// `p` (with `groups` capture groups) rewritten so that one more group
/// appended to it changes nothing: outside unicode mode, `\N` with
/// N = groups + 1 is an Annex B octal or identity escape, which the extra
/// group would turn into a backreference. Such an escape is spelled out
/// (`\xHH` for the octal part, then the remaining digits as literals).
fn pin_group_count_escapes(p: &[u32], groups: usize, unicode_sets: bool) -> Vec<u32> {
    let digit = |x: Option<&u32>| x.and_then(|&d| char::from_u32(d)?.to_digit(10));
    let mut out = Vec::with_capacity(p.len());
    let mut i = 0;
    let mut class_depth = 0usize;
    while i < p.len() {
        let c = p[i];
        if c == '\\' as u32 {
            if class_depth == 0 && matches!(digit(p.get(i + 1)), Some(1..=9)) {
                let mut end = i + 1;
                let mut value = 0usize;
                while let Some(d) = digit(p.get(end)) {
                    value = value.saturating_mul(10).saturating_add(d as usize);
                    end += 1;
                }
                if value == groups + 1 {
                    let mut j = i + 1;
                    let first = digit(p.get(j)).unwrap_or(0);
                    if first <= 7 {
                        // LegacyOctalEscapeSequence: up to three digits from
                        // 0-3, up to two from 4-7.
                        let max = if first <= 3 { 3 } else { 2 };
                        let mut octal = 0u32;
                        while j < end && j - (i + 1) < max && matches!(digit(p.get(j)), Some(0..=7)) {
                            octal = octal * 8 + digit(p.get(j)).unwrap_or(0);
                            j += 1;
                        }
                        out.extend(format!("\\x{octal:02X}").chars().map(|c| c as u32));
                    }
                    // An 8 or 9 (IdentityEscape) and any digits past the
                    // octal escape are literal.
                    out.extend_from_slice(&p[j..end]);
                    i = end;
                    continue;
                }
                out.extend_from_slice(&p[i..end]);
                i = end;
                continue;
            }
            out.push(c);
            if let Some(&n) = p.get(i + 1) {
                out.push(n);
            }
            i += 2;
            continue;
        }
        if class_depth > 0 {
            if c == ']' as u32 {
                class_depth -= 1;
            } else if c == '[' as u32 && unicode_sets {
                class_depth += 1;
            }
        } else if c == '[' as u32 {
            class_depth = 1;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Decode `\u{XXXX}` or `\uXXXX` (with a possible trailing surrogate
/// escape) at `p[i] == '\\'`; returns the code point and the next index.
fn name_escape(p: &[u32], i: usize) -> (u32, usize) {
    let hex = |j: usize, n: usize| -> Option<u32> {
        let mut v = 0u32;
        for k in 0..n {
            let d = char::from_u32(*p.get(j + k)?)?.to_digit(16)?;
            v = v * 16 + d;
        }
        Some(v)
    };
    if p.get(i + 1) != Some(&('u' as u32)) {
        return (p.get(i + 1).copied().unwrap_or(0), i + 2);
    }
    if p.get(i + 2) == Some(&('{' as u32)) {
        let mut j = i + 3;
        let mut v = 0u32;
        while j < p.len() && p[j] != '}' as u32 {
            v = v
                .saturating_mul(16)
                .saturating_add(char::from_u32(p[j]).and_then(|c| c.to_digit(16)).unwrap_or(0));
            j += 1;
        }
        return (v, j + 1);
    }
    let hi = hex(i + 2, 4).unwrap_or(0);
    if (0xD800..0xDC00).contains(&hi)
        && p.get(i + 6) == Some(&('\\' as u32))
        && p.get(i + 7) == Some(&('u' as u32))
    {
        if let Some(lo) = hex(i + 8, 4) {
            if (0xDC00..0xE000).contains(&lo) {
                return (0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00), i + 12);
            }
        }
    }
    (hi, i + 6)
}
