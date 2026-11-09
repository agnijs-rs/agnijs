//! Language tags: structural validity, canonicalisation (UTS 35 canonical
//! syntax plus alias replacement), likely-subtag expansion and the set of
//! locales the compiled ICU4X data covers.
//!
//! ICU4X's `Locale` parser is stricter than the `unicode_locale_id`
//! grammar ECMA-402 requires: it rejects 5–8 letter language subtags
//! (`posix`), repeated `-u-` keys (which UTS 35 resolves to the first)
//! and single-character private-use subtags. Tags are therefore tokenised
//! and canonicalised here; ICU4X contributes the alias tables for
//! language identifiers (`iw` → `he`, `sgn-GR` → `gss`, variant order),
//! the `rg`/`sd` subdivision aliases and likely subtags whenever the tag
//! is one it can parse.
//!
//! ECMA-402 treats a tag as *available* when the implementation has data
//! for it. ICU4X ships data per CLDR locale and falls back along the CLDR
//! parent chain when a request misses; the baked provider reports the
//! locale it actually resolved to. A tag counts as available here when the
//! data resolves exactly, or when the resolved locale and the request
//! maximise to the same likely subtags (`en-US` is served by `en`, `zh-TW`
//! by `zh-Hant`). Anything else is handled by ECMA-402's own lookup, which
//! strips subtags until it reaches an available tag.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use icu_locale::{LocaleCanonicalizer, LocaleExpander};
use icu_locale_core::{LanguageIdentifier, Locale};
use icu_provider::prelude::*;

/// The baked ICU4X data this crate enumerates to learn which locales exist.
/// The formatters themselves use their crates' own compiled data; this
/// provider only answers "is there data for this locale?".
use crate::provider::IntlData;

fn canonicalizer() -> &'static LocaleCanonicalizer {
    static C: OnceLock<LocaleCanonicalizer> = OnceLock::new();
    C.get_or_init(LocaleCanonicalizer::new_extended)
}

fn expander() -> &'static LocaleExpander {
    static E: OnceLock<LocaleExpander> = OnceLock::new();
    E.get_or_init(LocaleExpander::new_extended)
}

fn alnum(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric())
}
fn alpha(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphabetic())
}
fn digit(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}
fn is_variant(s: &str) -> bool {
    ((5..=8).contains(&s.len()) && alnum(s)) || (s.len() == 4 && s.as_bytes()[0].is_ascii_digit() && alnum(s))
}
fn is_tkey(p: &str) -> bool {
    p.len() == 2 && p.as_bytes()[0].is_ascii_alphabetic() && p.as_bytes()[1].is_ascii_digit()
}
fn is_ukey(p: &str) -> bool {
    p.len() == 2 && alnum(p) && p.as_bytes()[1].is_ascii_alphabetic()
}

/// Is `s` a syntactically valid language subtag (2–3 or 5–8 letters)?
pub fn is_language_subtag(s: &str) -> bool {
    alpha(s) && (s.len() == 2 || s.len() == 3 || (5..=8).contains(&s.len()))
}
pub fn is_script_subtag(s: &str) -> bool {
    s.len() == 4 && alpha(s)
}
pub fn is_region_subtag(s: &str) -> bool {
    (s.len() == 2 && alpha(s)) || (s.len() == 3 && digit(s))
}

/// The length of the `unicode_language_id` prefix of `subtags`, or `None`
/// when it is not one.
fn language_id_len(subtags: &[&str]) -> Option<usize> {
    if subtags.is_empty() || !is_language_subtag(subtags[0]) {
        return None;
    }
    let mut i = 1;
    if i < subtags.len() && is_script_subtag(subtags[i]) {
        i += 1;
    }
    if i < subtags.len() && is_region_subtag(subtags[i]) {
        i += 1;
    }
    let mut variants = BTreeSet::new();
    while i < subtags.len() && is_variant(subtags[i]) {
        if !variants.insert(subtags[i].to_ascii_lowercase()) {
            return None;
        }
        i += 1;
    }
    Some(i)
}

/// Is `body` a valid `tlang` followed by `tfield`s? Duplicate `tfield`
/// keys are rejected, as UTS 35 requires.
fn valid_t_body(body: &[&str]) -> bool {
    if !body.iter().all(|p| (2..=8).contains(&p.len()) && alnum(p)) {
        return false;
    }
    let mut j = 0;
    if j < body.len() && !is_tkey(body[j]) {
        let end = body.iter().position(|p| is_tkey(p)).unwrap_or(body.len());
        if language_id_len(&body[..end]) != Some(end) {
            return false;
        }
        j = end;
    }
    let mut keys = BTreeSet::new();
    while j < body.len() {
        if !is_tkey(body[j]) || !keys.insert(body[j].to_ascii_lowercase()) {
            return false;
        }
        j += 1;
        let vstart = j;
        while j < body.len() && body[j].len() >= 3 {
            j += 1;
        }
        if j == vstart {
            return false;
        }
    }
    true
}

/// Is `body` valid `-u-` content: attributes (3–8) then keys with types?
/// Repeated keys are allowed here and resolved to the first by
/// canonicalisation.
fn valid_u_body(body: &[&str]) -> bool {
    if !body.iter().all(|p| (2..=8).contains(&p.len()) && alnum(p)) {
        return false;
    }
    let mut j = 0;
    while j < body.len() && body[j].len() >= 3 {
        j += 1;
    }
    while j < body.len() {
        if !is_ukey(body[j]) {
            return false;
        }
        j += 1;
        while j < body.len() && body[j].len() >= 3 {
            j += 1;
        }
    }
    true
}

/// IsStructurallyValidLanguageTag: the `unicode_locale_id` grammar of
/// UTS 35 with no duplicate variants, singletons or `-t-` field keys.
pub fn is_structurally_valid(tag: &str) -> bool {
    if tag.is_empty() || !tag.is_ascii() {
        return false;
    }
    let subtags: Vec<&str> = tag.split('-').collect();
    if subtags.iter().any(|s| s.is_empty()) {
        return false;
    }
    let Some(mut i) = language_id_len(&subtags) else { return false };
    let mut singletons = BTreeSet::new();
    while i < subtags.len() {
        let singleton = subtags[i];
        if singleton.len() != 1 || !alnum(singleton) {
            return false;
        }
        let s = singleton.to_ascii_lowercase();
        if !singletons.insert(s.clone()) {
            return false;
        }
        i += 1;
        if s == "x" {
            let body = &subtags[i..];
            return !body.is_empty() && body.iter().all(|p| p.len() <= 8 && alnum(p));
        }
        let start = i;
        while i < subtags.len() && subtags[i].len() > 1 {
            i += 1;
        }
        let body = &subtags[start..i];
        if body.is_empty() {
            return false;
        }
        let ok = match s.as_str() {
            "u" => valid_u_body(body),
            "t" => valid_t_body(body),
            _ => body.iter().all(|p| (2..=8).contains(&p.len()) && alnum(p)),
        };
        if !ok {
            return false;
        }
    }
    true
}

/// A tokenised tag: all subtags lower-cased.
#[derive(Clone, Debug, Default)]
struct Tokens {
    /// `unicode_language_id` subtags.
    id: Vec<String>,
    /// Non-private extensions in input order: `(singleton, body)`.
    exts: Vec<(char, Vec<String>)>,
    /// The `-x-` subtags.
    private: Vec<String>,
}

fn tokenize(tag: &str) -> Tokens {
    let lower = tag.to_ascii_lowercase();
    let subtags: Vec<&str> = lower.split('-').collect();
    let n = language_id_len(&subtags).unwrap_or(subtags.len().min(1));
    let mut t = Tokens { id: subtags[..n].iter().map(|s| s.to_string()).collect(), ..Default::default() };
    let mut i = n;
    while i < subtags.len() {
        let s = subtags[i];
        i += 1;
        if s.len() != 1 {
            // Not a singleton: keep the tag usable by treating the rest as private.
            t.private.push(s.to_string());
            continue;
        }
        if s == "x" {
            t.private.extend(subtags[i..].iter().map(|p| p.to_string()));
            break;
        }
        let start = i;
        while i < subtags.len() && subtags[i].len() > 1 {
            i += 1;
        }
        t.exts.push((s.chars().next().unwrap(), subtags[start..i].iter().map(|p| p.to_string()).collect()));
    }
    t
}

impl Tokens {
    fn u_body(&self) -> Option<&Vec<String>> {
        self.exts.iter().find(|(c, _)| *c == 'u').map(|(_, b)| b)
    }

    /// Serialise with canonical extension order: singletons sorted, `x` last.
    fn to_tag(&self) -> String {
        let mut out = self.id.join("-");
        let mut exts: Vec<&(char, Vec<String>)> = self.exts.iter().filter(|(_, b)| !b.is_empty()).collect();
        exts.sort_by_key(|(c, _)| *c);
        for (c, body) in exts {
            out.push('-');
            out.push(*c);
            for p in body {
                out.push('-');
                out.push_str(p);
            }
        }
        if !self.private.is_empty() {
            out.push_str("-x");
            for p in &self.private {
                out.push('-');
                out.push_str(p);
            }
        }
        out
    }
}

/// Canonical casing (and ICU4X's variant ordering) for a language id.
fn canonical_id(id: &[String]) -> Vec<String> {
    let joined = id.join("-");
    if let Ok(lid) = LanguageIdentifier::try_from_str(&joined) {
        return lid.to_string().split('-').map(|s| s.to_string()).collect();
    }
    let mut out = Vec::with_capacity(id.len());
    let mut variants = Vec::new();
    for (i, s) in id.iter().enumerate() {
        if i == 0 {
            out.push(s.to_ascii_lowercase());
        } else if is_script_subtag(s) && i == 1 {
            let mut c = s.to_ascii_lowercase();
            c[..1].make_ascii_uppercase();
            out.push(c);
        } else if is_region_subtag(s) && out.len() <= 2 && variants.is_empty() {
            out.push(s.to_ascii_uppercase());
        } else {
            variants.push(s.to_ascii_lowercase());
        }
    }
    variants.sort();
    variants.dedup();
    out.extend(variants);
    out
}

/// Parsed `-u-` content: attributes and keywords with the first value of
/// each key.
struct UnicodeExt {
    attributes: Vec<String>,
    keywords: BTreeMap<String, String>,
}

fn parse_u(body: &[String]) -> UnicodeExt {
    let mut j = 0;
    let mut attributes = Vec::new();
    while j < body.len() && body[j].len() >= 3 {
        attributes.push(body[j].clone());
        j += 1;
    }
    attributes.sort();
    attributes.dedup();
    let mut keywords = BTreeMap::new();
    while j < body.len() {
        let key = body[j].clone();
        j += 1;
        let start = j;
        while j < body.len() && body[j].len() >= 3 {
            j += 1;
        }
        let value = body[start..j].join("-");
        keywords.entry(key).or_insert(value);
    }
    UnicodeExt { attributes, keywords }
}

fn serialize_u(u: &UnicodeExt) -> Vec<String> {
    let mut body = u.attributes.clone();
    for (k, v) in &u.keywords {
        body.push(k.clone());
        if !v.is_empty() {
            body.extend(v.split('-').map(|s| s.to_string()));
        }
    }
    body
}

/// UTS 35 §3.2.1: the canonical form of a `-u-` keyword value. Case is
/// lowered, `true` becomes the bare key, `yes` on the boolean keys does
/// the same, and the BCP 47 alias data for `ca`, `ks`, `ms` and `tz` is
/// applied.
pub fn canonicalize_keyword_value(key: &str, value: &str) -> String {
    let key = key.to_ascii_lowercase();
    let v = value.to_ascii_lowercase();
    if v == "true" || (v == "yes" && matches!(key.as_str(), "kb" | "kc" | "kh" | "kk" | "kn")) {
        return String::new();
    }
    let alias = match (key.as_str(), v.as_str()) {
        ("ca", "ethiopic-amete-alem") => "ethioaa",
        ("ca", "islamicc") => "islamic-civil",
        ("ca", "gregorian") => "gregory",
        ("ks", "primary") => "level1",
        ("ks", "tertiary") => "level3",
        ("ms", "imperial") => "uksystem",
        ("co", "gb2312han") => "gb2312",
        ("co", "phonebook") => "phonebk",
        ("co", "dictionary") => "dict",
        ("co", "traditional") => "trad",
        ("tz", "aqams") => "nzakl",
        ("tz", "cnckg") => "cnsha",
        ("tz", "cnhrb") => "cnsha",
        ("tz", "cnkhg") => "cnurc",
        ("tz", "cuba") => "cuhav",
        ("tz", "egypt") => "egcai",
        ("tz", "eire") => "iedub",
        ("tz", "est") => "papty",
        ("tz", "gmt0") => "gmt",
        ("tz", "hongkong") => "hkhkg",
        ("tz", "hst") => "ushnl",
        ("tz", "iceland") => "isrey",
        ("tz", "iran") => "irthr",
        ("tz", "israel") => "jeruslm",
        ("tz", "jamaica") => "jmkin",
        ("tz", "japan") => "jptyo",
        ("tz", "libya") => "lytip",
        ("tz", "mst") => "usphx",
        ("tz", "navajo") => "usden",
        ("tz", "poland") => "plwaw",
        ("tz", "portugal") => "ptlis",
        ("tz", "prc") => "cnsha",
        ("tz", "roc") => "twtpe",
        ("tz", "rok") => "krsel",
        ("tz", "singapore") => "sgsin",
        ("tz", "turkey") => "trist",
        ("tz", "uct") => "utc",
        ("tz", "usnavajo") => "usden",
        ("tz", "zulu") => "utc",
        _ => return v,
    };
    alias.to_string()
}

/// The canonical form of a `-t-` field value.
fn canonicalize_tvalue(key: &str, value: &str) -> String {
    let v = value.to_ascii_lowercase();
    match (key, v.as_str()) {
        ("m0", "names") => "prprname".to_string(),
        _ => v,
    }
}

fn canonical_t(body: &[String]) -> Vec<String> {
    let mut j = 0;
    let mut out = Vec::new();
    if j < body.len() && !is_tkey(&body[j]) {
        let end = body.iter().position(|p| is_tkey(p)).unwrap_or(body.len());
        out.extend(canonical_id(&body[..end]).into_iter().map(|s| s.to_ascii_lowercase()));
        j = end;
    }
    let mut fields = BTreeMap::new();
    while j < body.len() {
        let key = body[j].clone();
        j += 1;
        let start = j;
        while j < body.len() && body[j].len() >= 3 {
            j += 1;
        }
        let value = body[start..j].join("-");
        fields.entry(key).or_insert(value);
    }
    for (k, v) in fields {
        let v = canonicalize_tvalue(&k, &v);
        out.push(k);
        out.extend(v.split('-').filter(|s| !s.is_empty()).map(|s| s.to_string()));
    }
    out
}

/// Canonical syntax: casing, subtag order, duplicate-key removal and the
/// keyword aliases this module knows.
fn canonical_syntax(tag: &str) -> Tokens {
    let t = tokenize(tag);
    let mut out = Tokens { id: canonical_id(&t.id), ..Default::default() };
    for (c, body) in &t.exts {
        let body = match c {
            'u' => {
                let mut u = parse_u(body);
                for (k, v) in u.keywords.iter_mut() {
                    *v = canonicalize_keyword_value(k, v);
                }
                serialize_u(&u)
            }
            't' => canonical_t(body),
            _ => body.clone(),
        };
        out.exts.push((*c, body));
    }
    out.private = t.private;
    out
}

/// Parse a structurally valid tag into an ICU4X locale, when ICU4X can
/// represent it.
pub fn parse(tag: &str) -> Option<Locale> {
    if !is_structurally_valid(tag) {
        return None;
    }
    Locale::try_from_str(tag).ok()
}

/// CanonicalizeUnicodeLocaleId: structural validity, alias replacement for
/// language, script, region, variants and `-u-`/`-t-` values, canonical
/// casing and subtag order. `None` when the tag is not structurally valid.
pub fn canonicalize(tag: &str) -> Option<String> {
    if !is_structurally_valid(tag) {
        return None;
    }
    let mut s = canonical_syntax(tag).to_tag();
    // ICU4X supplies the language, variant, tlang and subdivision aliases
    // for every tag it can parse; its output is re-canonicalised so the
    // keyword aliases above apply to anything it rewrote.
    if let Ok(mut loc) = Locale::try_from_str(&s) {
        canonicalizer().canonicalize(&mut loc);
        s = canonical_syntax(&loc.to_string()).to_tag();
    }
    Some(s)
}

/// The language identifier (no extensions) of a tag.
pub fn base_name(tag: &str) -> String {
    canonical_id(&tokenize(tag).id).join("-")
}

/// Split a canonical tag into its remainder (language identifier plus the
/// other extensions) and the `-u-` extension keywords `(key, value)`; an
/// empty value is a bare key.
pub fn split_unicode_extension(tag: &str) -> (String, Vec<(String, String)>) {
    let mut t = canonical_syntax(tag);
    let keywords = match t.u_body() {
        Some(body) => parse_u(body).keywords.into_iter().collect(),
        None => Vec::new(),
    };
    t.exts.retain(|(c, _)| *c != 'u');
    (t.to_tag(), keywords)
}

/// Insert (or replace) `-u-` keywords into a tag; `None` values remove a key.
pub fn with_unicode_keywords(tag: &str, keywords: &[(&str, Option<&str>)]) -> String {
    let mut t = canonical_syntax(tag);
    let mut u = match t.u_body() {
        Some(body) => parse_u(body),
        None => UnicodeExt { attributes: Vec::new(), keywords: BTreeMap::new() },
    };
    for (k, v) in keywords {
        let key = k.to_ascii_lowercase();
        match v {
            Some(v) => {
                let value = v.to_ascii_lowercase();
                if !value.is_empty() && !is_unicode_type(&value) {
                    continue;
                }
                u.keywords.insert(key, value);
            }
            None => {
                u.keywords.remove(&key);
            }
        }
    }
    t.exts.retain(|(c, _)| *c != 'u');
    let body = serialize_u(&u);
    if !body.is_empty() {
        t.exts.push(('u', body));
    }
    t.to_tag()
}

/// Is `s` a valid `type` value for a `-u-` keyword (one or more 3–8
/// alphanumeric subtags)?
pub fn is_unicode_type(s: &str) -> bool {
    !s.is_empty() && s.split('-').all(|p| (3..=8).contains(&p.len()) && alnum(p))
}

fn expand(tag: &str, maximize_only: bool) -> String {
    let mut t = canonical_syntax(tag);
    let joined = t.id.join("-");
    let Ok(mut lid) = LanguageIdentifier::try_from_str(&joined) else {
        return t.to_tag();
    };
    // CLDR 44+ never leaves `und` in a maximized tag: when no likely
    // subtag entry matches, the root mapping `en-Latn-US` fills the gaps
    // (`und-150` -> `en-Latn-150`). ICU4X stops at the data lookup, so
    // the root step is supplied here, and minimizing goes through the
    // maximized form the same way.
    fn fill_root(lid: &mut LanguageIdentifier) {
        if lid.language.is_unknown() {
            lid.language = icu_locale_core::subtags::language!("en");
            if lid.script.is_none() {
                lid.script = Some(icu_locale_core::subtags::script!("Latn"));
            }
            if lid.region.is_none() {
                lid.region = Some(icu_locale_core::subtags::region!("US"));
            }
        }
    }
    if maximize_only {
        expander().maximize(&mut lid);
        fill_root(&mut lid);
    } else if lid.language.is_unknown() {
        expander().maximize(&mut lid);
        fill_root(&mut lid);
        expander().minimize(&mut lid);
    } else {
        expander().minimize(&mut lid);
    }
    t.id = lid.to_string().split('-').map(|s| s.to_string()).collect();
    t.to_tag()
}

/// AddLikelySubtags, keeping extensions.
pub fn maximize(tag: &str) -> String {
    expand(tag, true)
}

/// RemoveLikelySubtags, keeping extensions.
pub fn minimize(tag: &str) -> String {
    expand(tag, false)
}

struct Available {
    exact: BTreeSet<String>,
    maximized: BTreeSet<String>,
}

fn available() -> &'static Available {
    static A: OnceLock<Available> = OnceLock::new();
    A.get_or_init(|| {
        let mut exact = BTreeSet::new();
        let mut add = |ids: Result<BTreeSet<DataIdentifierCow<'static>>, DataError>| {
            for id in ids.unwrap_or_default() {
                if id.locale.is_unknown() {
                    continue;
                }
                let loc = id.locale.into_locale();
                if !loc.id.variants.is_empty() {
                    continue;
                }
                exact.insert(loc.to_string());
            }
        };
        add(IterableDataProvider::<icu_decimal::provider::DecimalSymbolsV1>::iter_ids(&IntlData));
        add(IterableDataProvider::<icu_datetime::provider::names::DatetimeNamesMonthGregorianV1>::iter_ids(&IntlData));
        add(IterableDataProvider::<icu_datetime::provider::semantic_skeletons::DatetimePatternsDateGregorianV1>::iter_ids(&IntlData));
        add(IterableDataProvider::<icu_list::provider::ListAndV1>::iter_ids(&IntlData));
        add(IterableDataProvider::<icu_plurals::provider::PluralsCardinalV1>::iter_ids(&IntlData));
        let maximized = exact.iter().map(|t| maximize(t)).collect();
        Available { exact, maximized }
    })
}

/// Does the compiled data serve `tag` (a language identifier without
/// extensions) either directly or through a locale with the same likely
/// subtags?
pub fn is_available(tag: &str) -> bool {
    let t = canonical_syntax(tag);
    if !t.exts.is_empty() || !t.private.is_empty() || parts(tag).variants.len() > 0 {
        return false;
    }
    // A lone singleton (`de-x`) tokenizes to just the language id.
    if t.id.len() != tag.split('-').count() {
        return false;
    }
    if LanguageIdentifier::try_from_str(&t.id.join("-")).is_err() {
        return false;
    }
    let a = available();
    let base = t.id.join("-");
    a.exact.contains(&base) || a.maximized.contains(&maximize(&base))
}

/// Every locale the compiled data has an entry for, as canonical tags.
pub fn data_locales() -> Vec<String> {
    available().exact.iter().cloned().collect()
}

/// Language-tag pieces of a canonical tag, for `Intl.Locale`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parts {
    pub language: String,
    pub script: Option<String>,
    pub region: Option<String>,
    pub variants: Vec<String>,
}

pub fn parts(tag: &str) -> Parts {
    let id = canonical_id(&tokenize(tag).id);
    let mut p = Parts::default();
    let mut i = 0;
    if let Some(l) = id.first() {
        p.language = l.clone();
        i = 1;
    }
    if i < id.len() && is_script_subtag(&id[i]) {
        p.script = Some(id[i].clone());
        i += 1;
    }
    if i < id.len() && is_region_subtag(&id[i]) {
        p.region = Some(id[i].clone());
        i += 1;
    }
    p.variants = id[i..].to_vec();
    p
}

/// Replace language-identifier pieces of a canonical tag, keeping its
/// variants and extensions. Returns `None` when a piece is not a valid
/// subtag.
pub fn with_parts(tag: &str, language: Option<&str>, script: Option<&str>, region: Option<&str>) -> Option<String> {
    let mut t = canonical_syntax(tag);
    let p = parts(tag);
    let lang = language.unwrap_or(&p.language);
    if !is_language_subtag(lang) {
        return None;
    }
    let script = match script {
        Some(s) => {
            if !is_script_subtag(s) {
                return None;
            }
            Some(s.to_string())
        }
        None => p.script.clone(),
    };
    let region = match region {
        Some(r) => {
            if !is_region_subtag(r) {
                return None;
            }
            Some(r.to_string())
        }
        None => p.region.clone(),
    };
    let mut id = vec![lang.to_string()];
    id.extend(script);
    id.extend(region);
    id.extend(p.variants.iter().cloned());
    t.id = canonical_id(&id);
    Some(t.to_tag())
}

/// Text direction for `Intl.Locale.prototype.getTextInfo`.
pub fn is_right_to_left(tag: &str) -> bool {
    let Ok(lid) = LanguageIdentifier::try_from_str(&base_name(tag)) else {
        return false;
    };
    static D: OnceLock<icu_locale::LocaleDirectionality> = OnceLock::new();
    let d = D.get_or_init(icu_locale::LocaleDirectionality::new_common);
    matches!(d.get(&lid), Some(icu_locale::Direction::RightToLeft))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_case_and_aliases() {
        assert_eq!(canonicalize("EN-us").as_deref(), Some("en-US"));
        assert_eq!(canonicalize("iw").as_deref(), Some("he"));
        assert_eq!(canonicalize("zh-hans-cn").as_deref(), Some("zh-Hans-CN"));
        assert_eq!(canonicalize("en-u-ca-gregory").as_deref(), Some("en-u-ca-gregory"));
        assert_eq!(canonicalize("en-u-kb-true").as_deref(), Some("en-u-kb"));
        assert_eq!(canonicalize("sgn-GR").as_deref(), Some("gss"));
        assert_eq!(canonicalize("und-u-ca-ethiopic-amete-alem").as_deref(), Some("und-u-ca-ethioaa"));
        assert_eq!(canonicalize("und-u-ks-primary").as_deref(), Some("und-u-ks-level1"));
        assert_eq!(canonicalize("und-u-tz-cnckg").as_deref(), Some("und-u-tz-cnsha"));
        assert_eq!(canonicalize("und-u-kb-yes").as_deref(), Some("und-u-kb"));
        assert_eq!(canonicalize("und-u-ka-yes").as_deref(), Some("und-u-ka-yes"));
        assert_eq!(canonicalize("und-Latn-t-und-hani-m0-names").as_deref(), Some("und-Latn-t-und-hani-m0-prprname"));
        assert_eq!(canonicalize("DE-T-M0-DIN-K0-QWERTZ").as_deref(), Some("de-t-k0-qwertz-m0-din"));
        assert_eq!(canonicalize("en-t-iw").as_deref(), Some("en-t-he"));
        assert_eq!(canonicalize("cmn-hans-cn-u-ca-t-ca-x-t-u").as_deref(), Some("zh-Hans-CN-t-ca-u-ca-x-t-u"));
        assert_eq!(canonicalize("da-u-ca-gregory-ca-buddhist").as_deref(), Some("da-u-ca-gregory"));
        assert_eq!(canonicalize("pt-u-attr2-attr1-ca-gregory").as_deref(), Some("pt-u-attr1-attr2-ca-gregory"));
        assert_eq!(canonicalize("posix").as_deref(), Some("posix"));
        assert_eq!(canonicalize("en-x-u-foo").as_deref(), Some("en-x-u-foo"));
        assert_eq!(canonicalize("en-u-baz-a-bar-x-u-foo").as_deref(), Some("en-a-bar-u-baz-x-u-foo"));
        assert_eq!(canonicalize("art-lojban").as_deref(), Some("jbo"));
        assert_eq!(canonicalize("sl-rozaj-biske-1994").as_deref(), Some("sl-1994-biske-rozaj"));
        assert_eq!(
            canonicalize("de-latn-de-fonipa-1996-u-ca-gregory-co-phonebk-hc-h23-kf-true-kn-false-nu-latn").as_deref(),
            Some("de-Latn-DE-1996-fonipa-u-ca-gregory-co-phonebk-hc-h23-kf-kn-false-nu-latn")
        );
        assert_eq!(canonicalize("und-u-rg-no23").as_deref(), Some("und-u-rg-no50"));
    }

    #[test]
    fn rejects_invalid_tags() {
        for t in [
            "", "i", "x", "u", "419", "hans", "de-gregory-gregory", "de-1996-1996", "de_DE",
            "pt-u-ca-gregory-u-nu-latn", "cmn-hans-cn-u-u", "de-u", "de-u-ca-", "x-foo",
            "en\u{0}", " en", "en ", "it-IT-Latn", "es-Latn-latn", "pl-PL-pl", "i-ami", "en-GB-oed",
            "sgn-BE-FR", "no-bok", "zh-min-nan", "en-t-d0", "en-t-en-latn-latn", "en-t-ar-aao",
        ] {
            assert!(canonicalize(t).is_none(), "{t:?} should be invalid");
        }
        for t in [
            "en", "en-US", "zh-Hant-TW", "de-DE-u-co-phonebk", "en-x-private", "en-t-de-m0-ascii",
            "en-u-ca-gregory-kb", "de-x-u-co-phonebk", "jbo-x-0", "en-t-m0-true",
        ] {
            assert!(canonicalize(t).is_some(), "{t:?} should be valid");
        }
    }

    #[test]
    fn availability_follows_data() {
        assert!(is_available("en"));
        assert!(is_available("en-US"));
        assert!(is_available("zh-TW"));
        assert!(is_available("de-CH"));
        assert!(!is_available("en-XX"));
        assert!(!is_available("tlh"));
        assert!(!is_available("zxx"));
        assert!(!is_available("posix"));
        assert!(data_locales().iter().any(|l| l == "en"));
    }

    #[test]
    fn likely_subtags() {
        assert_eq!(maximize("en"), "en-Latn-US");
        assert_eq!(minimize("en-Latn-US"), "en");
        assert_eq!(maximize("zh-TW"), "zh-Hant-TW");
        assert_eq!(maximize("und"), "en-Latn-US");
        assert_eq!(minimize("und"), "en");
        assert_eq!(maximize("und-fonipa-u-co-phonebk"), "en-Latn-US-fonipa-u-co-phonebk");
        assert_eq!(minimize("und-Latn-AQ"), "en-AQ");
        assert_eq!(maximize("und-Thai"), "th-Thai-TH");
    }

    #[test]
    fn extension_split_and_join() {
        let (base, kw) = split_unicode_extension("de-DE-u-co-phonebk-nu-latn-x-priv");
        assert_eq!(base, "de-DE-x-priv");
        assert_eq!(kw, vec![("co".into(), "phonebk".into()), ("nu".into(), "latn".into())]);
        assert_eq!(
            with_unicode_keywords("en-US", &[("ca", Some("gregory")), ("nu", Some("latn"))]),
            "en-US-u-ca-gregory-nu-latn"
        );
        assert_eq!(with_unicode_keywords("en-US-u-ca-gregory", &[("ca", None)]), "en-US");
        assert_eq!(base_name("de-Latn-DE-1996-fonipa-u-ca-gregory"), "de-Latn-DE-1996-fonipa");
        let p = parts("de-Latn-DE-1996-fonipa-u-ca-gregory");
        assert_eq!(p.script.as_deref(), Some("Latn"));
        assert_eq!(p.region.as_deref(), Some("DE"));
        assert_eq!(p.variants, vec!["1996", "fonipa"]);
        assert_eq!(with_parts("en-US-u-ca-gregory", Some("fr"), None, Some("CA")).as_deref(), Some("fr-CA-u-ca-gregory"));
    }
}
