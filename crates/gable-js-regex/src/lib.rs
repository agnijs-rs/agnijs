//! gable-js-regex: regular expressions for the Gable engine.
//!
//! The `syntax` module validates regular expression literals at parse
//! time (ES2025 pattern grammar plus Annex B). [`Regex`] is the matcher
//! seam the VM codes against: compile a pattern under a set of flags, then
//! run it against UTF-16 code units from a start index. The engine behind
//! the seam is `regress` (ADR 0013); nothing outside this crate names it.

pub mod syntax;

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

/// A compiled pattern.
pub struct Regex {
    inner: regress::Regex,
    /// `(?:P)()|` for sticky attempts, compiled on first use (see
    /// [`Regex::exec_sticky`]).
    anchored: std::cell::OnceCell<regress::Regex>,
    /// The numbered-groups pattern and backend flags `inner` came from.
    source: Vec<u32>,
    backend_flags: regress::Flags,
    unicode_mode: bool,
    /// Capture-group names by group number (1-based groups at index
    /// `n - 1`); `None` for unnamed groups.
    names: Vec<Option<String>>,
}

/// One successful match: `ranges[0]` is the whole match, `ranges[n]` the
/// n-th capture (`None` when the group did not participate). All indices
/// are UTF-16 code-unit offsets into the input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Captures {
    pub ranges: Vec<Option<(usize, usize)>>,
}

impl Regex {
    /// Compile `pattern` (UTF-16 code units, as the source text or
    /// `ToString(pattern)` gives it) under `flags`. The error message is
    /// what the `SyntaxError` carries.
    pub fn compile(pattern: &[u16], flags: &Flags) -> Result<Regex, String> {
        let unicode_mode = flags.unicode_mode();
        // Our own grammar check first, so literals and `new RegExp` report
        // the same errors.
        syntax::validate_units(pattern, flags.unicode, flags.unicode_sets)?;
        let code_points = pattern_code_points(pattern, unicode_mode);
        let rflags = regress::Flags {
            icase: flags.ignore_case,
            multiline: flags.multiline,
            dot_all: flags.dot_all,
            no_opt: false,
            unicode: flags.unicode,
            unicode_sets: flags.unicode_sets,
        };
        let names = group_names(&code_points, unicode_mode);
        // The matcher sees only numbered groups: names and `\k<name>`
        // references are resolved here (ES2025 duplicate-name semantics
        // included), so the backend needs no named-group support.
        let stripped = strip_group_names(&code_points, unicode_mode, &names);
        let inner = regress::Regex::from_unicode(stripped.iter().copied(), rflags)
            .map_err(|e| format!("Invalid regular expression: {}", e.text))?;
        Ok(Regex {
            inner,
            anchored: std::cell::OnceCell::new(),
            source: stripped,
            backend_flags: rflags,
            unicode_mode,
            names,
        })
    }

    /// Number of capturing groups (not counting the whole match).
    pub fn group_count(&self) -> usize {
        self.names.len()
    }

    /// Capture-group names, index `i` for group `i + 1`.
    pub fn group_names(&self) -> &[Option<String>] {
        &self.names
    }

    /// Whether any group is named.
    pub fn has_named_groups(&self) -> bool {
        self.names.iter().any(|n| n.is_some())
    }

    /// Find the leftmost match starting at or after `start`.
    pub fn exec(&self, input: &[u16], start: usize) -> Option<Captures> {
        let m = self.find(&self.inner, input, start)?;
        Some(self.captures(&m, m.captures.len()))
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

    /// The match's whole range and its first `groups` captures.
    fn captures(&self, m: &regress::Match, groups: usize) -> Captures {
        let mut ranges = Vec::with_capacity(groups + 1);
        ranges.push(Some((m.range.start, m.range.end)));
        for c in &m.captures[..groups] {
            ranges.push(c.as_ref().map(|r| (r.start, r.end)));
        }
        // Groups the parser saw but the matcher did not report (it never
        // happens, but keep the arrays the same length either way).
        while ranges.len() < self.names.len() + 1 {
            ranges.push(None);
        }
        Captures { ranges }
    }

    /// Find a match that starts exactly at `start` (sticky semantics).
    ///
    /// The backend only searches leftmost, so a plain search would walk the
    /// rest of the input before failing: quadratic for a tokenizer probing
    /// every position. The search runs `(?:P)()|` instead: at `start`
    /// either `P` matches, and the trailing empty group (numbered after all
    /// of `P`'s groups) participates, or the empty alternative matches
    /// right there. Either way the search stops at `start`.
    pub fn exec_sticky(&self, input: &[u16], start: usize) -> Option<Captures> {
        let anchored = self.anchored.get_or_init(|| {
            let mut p: Vec<u32> = "(?:".chars().map(|c| c as u32).collect();
            p.extend_from_slice(&self.source);
            p.extend(")()|".chars().map(|c| c as u32));
            regress::Regex::from_unicode(p.into_iter(), self.backend_flags)
                .expect("a pattern that compiled still compiles inside a group")
        });
        let m = self.find(anchored, input, start)?;
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
    fn a_failed_sticky_match_does_not_search_the_rest_of_the_input() {
        // A tokenizer probes `/[a-z]+/y` at every position of a long line;
        // each failed probe must cost O(1), not O(rest of the input). The
        // old leftmost-search-then-filter took minutes here, the anchored
        // attempt takes milliseconds even in a debug build.
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
        assert_eq!(
            re.group_names(),
            &[Some("y".into()), Some("y".into()), None]
        );
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
