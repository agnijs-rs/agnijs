//! Unicode semantics of the regex engine (TASK-100.31): property escapes,
//! properties of strings and case folding, over the generated tables in
//! `unicode_tables.rs` (Unicode 17, the version test262's property-escape
//! tests are generated from).

use std::sync::OnceLock;

use crate::charset::{CharSet, MAX_CODE_POINT};
use crate::unicode_tables as t;

pub(crate) use t::UNICODE_VERSION;

/// Members of the General_Category groups, by short name.
const GC_GROUPS: &[(&str, &[&str])] = &[
    ("C", &["Cc", "Cf", "Cn", "Co", "Cs"]),
    ("L", &["Lu", "Ll", "Lt", "Lm", "Lo"]),
    ("LC", &["Lu", "Ll", "Lt"]),
    ("M", &["Mn", "Mc", "Me"]),
    ("N", &["Nd", "Nl", "No"]),
    ("P", &["Pc", "Pd", "Ps", "Pe", "Pi", "Pf", "Po"]),
    ("S", &["Sm", "Sc", "Sk", "So"]),
    ("Z", &["Zs", "Zl", "Zp"]),
];

fn gc_short(value: &str) -> Option<&'static str> {
    t::GENERAL_CATEGORY_ALIASES
        .iter()
        .find(|a| a.contains(&value))
        .map(|a| a[0])
}

fn general_category(value: &str) -> Option<CharSet> {
    let short = gc_short(value)?;
    let one = |s: &str| {
        t::GENERAL_CATEGORY
            .iter()
            .find(|(n, _)| *n == s)
            .map(|(_, r)| *r)
    };
    if let Some(r) = one(short) {
        return Some(CharSet::from_sorted(r));
    }
    let (_, members) = GC_GROUPS.iter().find(|(g, _)| *g == short)?;
    let mut v = Vec::new();
    for m in *members {
        v.extend_from_slice(one(m)?);
    }
    Some(CharSet::from_unsorted(v))
}

fn script(value: &str, extensions: bool) -> Option<CharSet> {
    let (_, sc, scx) = t::SCRIPTS.iter().find(|(a, _, _)| a.contains(&value))?;
    Some(CharSet::from_sorted(if extensions { scx } else { sc }))
}

fn binary(name: &str) -> Option<CharSet> {
    match name {
        "Any" => return Some(CharSet::range(0, MAX_CODE_POINT)),
        "ASCII" => return Some(CharSet::range(0, 0x7F)),
        "Assigned" => return general_category("Cn").map(|s| s.complement(MAX_CODE_POINT)),
        _ => {}
    }
    t::BINARY
        .iter()
        .find(|(a, _)| a.contains(&name))
        .map(|(_, r)| CharSet::from_sorted(r))
}

/// The code points of `\p{name}` or `\p{name=value}`, or `None` when the
/// escape names no property ES2025 allows (property and value names are
/// case-sensitive and only the listed aliases count).
pub(crate) fn property(name: &str, value: Option<&str>) -> Option<CharSet> {
    match value {
        None => general_category(name).or_else(|| binary(name)),
        Some(v) => match name {
            "General_Category" | "gc" => general_category(v),
            "Script" | "sc" => script(v, false),
            "Script_Extensions" | "scx" => script(v, true),
            _ => None,
        },
    }
}

/// Whether `\p{name}` names a property of strings.
pub(crate) fn is_string_property(name: &str) -> bool {
    name == "RGI_Emoji" || t::STRING_PROPERTIES.iter().any(|(n, _, _)| *n == name)
}

/// A property of strings (v mode): its single code points and its longer
/// strings.
pub(crate) fn string_property(name: &str) -> Option<(CharSet, Vec<Vec<u32>>)> {
    let mut singles = Vec::new();
    let mut strings = Vec::new();
    let mut found = false;
    for (n, s, l) in t::STRING_PROPERTIES {
        if *n == name || name == "RGI_Emoji" {
            found = true;
            singles.extend_from_slice(s);
            strings.extend(l.iter().map(|x| x.to_vec()));
        }
    }
    found.then(|| (CharSet::from_unsorted(singles), strings))
}

fn lookup(table: &[(u32, u32)], c: u32) -> u32 {
    match table.binary_search_by_key(&c, |&(k, _)| k) {
        Ok(i) => table[i].1,
        Err(_) => c,
    }
}

/// Canonicalize(rer, ch) for a pattern with the i flag: simple case
/// folding in unicode mode, the one-unit toUppercase otherwise.
#[inline]
pub(crate) fn canonicalize(c: u32, unicode: bool) -> u32 {
    if c < 128 {
        // ASCII folds to lower case in unicode mode, upper case otherwise.
        let b = c as u8;
        return if unicode {
            b.to_ascii_lowercase() as u32
        } else {
            b.to_ascii_uppercase() as u32
        };
    }
    if unicode {
        lookup(t::SIMPLE_FOLD, c)
    } else {
        lookup(t::CANON_UPPER, c)
    }
}

/// The characters that canonicalize alike, as (canonical value, members)
/// with the members sorted; only classes of two or more.
struct Classes {
    classes: Vec<(u32, Vec<u32>)>,
}

fn classes(unicode: bool) -> &'static Classes {
    static U: OnceLock<Classes> = OnceLock::new();
    static L: OnceLock<Classes> = OnceLock::new();
    let cell = if unicode { &U } else { &L };
    cell.get_or_init(|| {
        let table = if unicode {
            t::SIMPLE_FOLD
        } else {
            t::CANON_UPPER
        };
        let mut map: std::collections::BTreeMap<u32, Vec<u32>> = std::collections::BTreeMap::new();
        let mut add = |c: u32| {
            let k = canonicalize(c, unicode);
            map.entry(k).or_default().push(c);
        };
        for c in (b'A'..=b'Z').chain(b'a'..=b'z') {
            add(c as u32);
        }
        for &(c, k) in table {
            add(c);
            if canonicalize(k, unicode) == k {
                add(k);
            }
        }
        let classes = map
            .into_iter()
            .filter_map(|(k, mut v)| {
                v.sort_unstable();
                v.dedup();
                (v.len() > 1).then_some((k, v))
            })
            .collect();
        Classes { classes }
    })
}

/// Every character that canonicalizes like some member of `set`, so that
/// "some member canonicalizes like ch" becomes plain membership.
pub(crate) fn case_closure(set: &CharSet, unicode: bool) -> CharSet {
    let mut extra = Vec::new();
    for (_, members) in &classes(unicode).classes {
        if members.iter().any(|&m| set.contains(m)) {
            extra.extend(
                members
                    .iter()
                    .filter(|&&m| !set.contains(m))
                    .map(|&m| (m, m)),
            );
        }
    }
    if extra.is_empty() {
        return set.clone();
    }
    extra.extend_from_slice(set.ranges());
    CharSet::from_unsorted(extra)
}

/// The characters that canonicalize like `c` (at least `c` itself).
pub(crate) fn case_class(c: u32, unicode: bool) -> CharSet {
    let k = canonicalize(c, unicode);
    let cl = &classes(unicode).classes;
    match cl.binary_search_by_key(&k, |(key, _)| *key) {
        Ok(i) => CharSet::from_unsorted(cl[i].1.iter().map(|&m| (m, m)).collect()),
        Err(_) => CharSet::single(c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_the_unicode_version_test262_expects() {
        assert_eq!(UNICODE_VERSION, "17.0.0");
    }

    #[test]
    fn properties_resolve_by_every_alias() {
        let lu = property("Lu", None).unwrap();
        assert!(lu.contains('A' as u32) && !lu.contains('a' as u32));
        assert_eq!(property("Uppercase_Letter", None), Some(lu.clone()));
        assert_eq!(property("gc", Some("Lu")), Some(lu.clone()));
        assert_eq!(
            property("General_Category", Some("Uppercase_Letter")),
            Some(lu)
        );
        let l = property("L", None).unwrap();
        assert!(l.contains('a' as u32) && l.contains(0x4E00) && !l.contains('1' as u32));
        assert!(property("Script", Some("Greek")).unwrap().contains(0x3B1));
        assert_eq!(
            property("sc", Some("Grek")),
            property("Script", Some("Greek"))
        );
        // U+0342 is Inherited by script but Greek by extension.
        assert!(!property("sc", Some("Grek")).unwrap().contains(0x342));
        assert!(property("scx", Some("Grek")).unwrap().contains(0x342));
        assert!(property("Any", None).unwrap().contains(0x10FFFF));
        assert!(property("ASCII", None).unwrap().contains(0x7F));
        assert!(!property("Assigned", None).unwrap().contains(0x378));
        assert!(property("Alpha", None).unwrap().contains('z' as u32));
        assert!(property("space", None).unwrap().contains(0x3000));
        assert!(property("Bogus", None).is_none());
        assert!(property("Script", None).is_none());
        assert!(property("lu", None).is_none(), "names are case-sensitive");
        assert!(property("Lu", Some("x")).is_none());
    }

    #[test]
    fn properties_of_strings() {
        assert!(is_string_property("RGI_Emoji") && is_string_property("Basic_Emoji"));
        assert!(!is_string_property("Emoji"));
        let (singles, strings) = string_property("Emoji_Keycap_Sequence").unwrap();
        assert!(singles.is_empty());
        assert!(strings.contains(&vec!['#' as u32, 0xFE0F, 0x20E3]));
        let (singles, strings) = string_property("RGI_Emoji").unwrap();
        assert!(singles.contains(0x1F600));
        assert!(strings.len() > 2000, "{}", strings.len());
    }

    #[test]
    fn canonicalize_follows_the_mode() {
        assert_eq!(canonicalize('a' as u32, true), 'a' as u32);
        assert_eq!(canonicalize('A' as u32, true), 'a' as u32);
        assert_eq!(canonicalize('a' as u32, false), 'A' as u32);
        // The Kelvin sign and long s fold into ASCII only in unicode mode.
        assert_eq!(canonicalize(0x212A, true), 'k' as u32);
        assert_eq!(canonicalize(0x17F, true), 's' as u32);
        assert_eq!(canonicalize(0x17F, false), 0x17F);
        // Sharp s uppercases to two characters, so it stays.
        assert_eq!(canonicalize(0xDF, false), 0xDF);
        assert_eq!(canonicalize(0x3C2, false), 0x3A3);
        assert_eq!(
            canonicalize(0x1F80, false),
            0x1F80,
            "full uppercase is two characters"
        );
    }

    #[test]
    fn case_closure_adds_every_character_that_folds_alike() {
        let k = case_class('k' as u32, true);
        assert_eq!(k.ranges(), &[(0x4B, 0x4B), (0x6B, 0x6B), (0x212A, 0x212A)]);
        let k = case_class('k' as u32, false);
        assert_eq!(k.ranges(), &[(0x4B, 0x4B), (0x6B, 0x6B)]);
        let s = case_closure(&CharSet::range('a' as u32, 'z' as u32), true);
        assert!(s.contains('Q' as u32) && s.contains(0x17F) && s.contains(0x212A));
        let s = case_closure(&CharSet::single(0x3A3), false);
        assert_eq!(s.ranges(), &[(0x3A3, 0x3A3), (0x3C2, 0x3C3)]);
    }

    #[test]
    fn case_tables_match_a_full_sweep() {
        // Every character whose canonical form differs is in a class.
        for unicode in [false, true] {
            let max = if unicode { MAX_CODE_POINT } else { 0xFFFF };
            for c in 0..=max {
                let k = canonicalize(c, unicode);
                if k != c {
                    assert!(
                        case_class(c, unicode).contains(k) || canonicalize(k, unicode) != k,
                        "{c:x}"
                    );
                    assert!(case_class(c, unicode).contains(c), "{c:x}");
                }
            }
        }
    }
}
