//! `Intl.DisplayNames`: language, region, script and variant names from
//! ICU4X; currency names from ICU4X's currency data; calendar and
//! date-time-field names from the bundled CLDR tables.

use icu_experimental::displaynames::multi::{LanguageDisplayNames, LocaleDisplayNamesFormatter, RegionDisplayNames, ScriptDisplayNames, VariantDisplayNames};
use icu_experimental::displaynames::{DisplayNamesOptions, DisplayNamesPreferences, Fallback, LanguageDisplay, Style};
use icu_locale_core::subtags::{Language, Region, Script, Variant};
use icu_locale_core::Locale;

use crate::cldr_tables::{DisplayNamesLocale, DISPLAY_NAMES};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameType {
    Language,
    Region,
    Script,
    Currency,
    Calendar,
    DateTimeField,
}

impl NameType {
    pub fn parse(s: &str) -> Option<NameType> {
        Some(match s {
            "language" => NameType::Language,
            "region" => NameType::Region,
            "script" => NameType::Script,
            "currency" => NameType::Currency,
            "calendar" => NameType::Calendar,
            "dateTimeField" => NameType::DateTimeField,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameStyle {
    Long,
    Short,
    Narrow,
}

pub struct IntlDisplayNames {
    locale: Locale,
    tag: String,
    ty: NameType,
    style: NameStyle,
    dialect: bool,
}

/// The outcome of `of(code)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lookup {
    /// A display name was found.
    Name(String),
    /// No name; the canonicalized code (for `fallback: "code"`).
    Code(String),
    /// The code is not well-formed for the type (RangeError).
    Invalid,
}

pub const DATE_TIME_FIELDS: &[&str] = &[
    "era", "year", "quarter", "month", "weekOfYear", "weekday", "day", "dayPeriod", "hour", "minute", "second", "timeZoneName",
];

impl IntlDisplayNames {
    pub fn new(tag: &str, ty: NameType, style: NameStyle, dialect: bool) -> Result<IntlDisplayNames, String> {
        let locale: Locale = crate::locale::base_name(tag).parse().map_err(|_| format!("bad locale {tag}"))?;
        Ok(IntlDisplayNames { locale, tag: tag.to_string(), ty, style, dialect })
    }

    fn options(&self) -> DisplayNamesOptions {
        let mut o = DisplayNamesOptions::default();
        o.style = Some(match self.style {
            NameStyle::Long => Style::Long,
            NameStyle::Short => Style::Short,
            NameStyle::Narrow => Style::Narrow,
        });
        o.fallback = Fallback::None;
        o.language_display = if self.dialect { LanguageDisplay::Dialect } else { LanguageDisplay::Standard };
        o
    }

    fn prefs(&self) -> DisplayNamesPreferences {
        DisplayNamesPreferences::from(&self.locale)
    }

    fn table(&self) -> Option<&'static DisplayNamesLocale> {
        for cand in crate::number::table_candidates(&self.tag) {
            if let Some((_, t)) = DISPLAY_NAMES.iter().find(|(l, _)| *l == cand) {
                return Some(t);
            }
        }
        None
    }

    pub fn of(&self, code: &str) -> Lookup {
        match self.ty {
            NameType::Language => self.of_language(code),
            NameType::Region => {
                let Some(region) = parse_region(code) else { return Lookup::Invalid };
                let canonical = region.to_string();
                let names = match RegionDisplayNames::try_new(self.prefs(), self.options()) {
                    Ok(n) => n,
                    Err(_) => return Lookup::Code(canonical),
                };
                match names.of(region) {
                    Some(n) => Lookup::Name(n.to_string()),
                    None => Lookup::Code(canonical),
                }
            }
            NameType::Script => {
                let Some(script) = parse_script(code) else { return Lookup::Invalid };
                let canonical = script.to_string();
                let names = match ScriptDisplayNames::try_new(self.prefs(), self.options()) {
                    Ok(n) => n,
                    Err(_) => return Lookup::Code(canonical),
                };
                match names.of(script) {
                    Some(n) => Lookup::Name(n.to_string()),
                    None => Lookup::Code(canonical),
                }
            }
            NameType::Currency => {
                if !crate::data::is_well_formed_currency(code) {
                    return Lookup::Invalid;
                }
                let canonical = code.to_ascii_uppercase();
                match crate::number::currency_display_name(&self.locale, &canonical) {
                    Some(n) => Lookup::Name(n),
                    // CLDR's root display name of a currency is its code.
                    None if crate::data::currencies().iter().any(|c| *c == canonical) => Lookup::Name(canonical),
                    None => Lookup::Code(canonical),
                }
            }
            NameType::Calendar => {
                if !crate::locale::is_unicode_type(code) {
                    return Lookup::Invalid;
                }
                let canonical = code.to_ascii_lowercase();
                // CLDR keys calendars by their long BCP-47 names.
                let key = match crate::data::canonical_calendar(&canonical).as_str() {
                    "gregory" => "gregorian".to_string(),
                    "ethioaa" => "ethiopic-amete-alem".to_string(),
                    k => k.to_string(),
                };
                if let Some(t) = self.table() {
                    if let Some((_, n)) = t.calendar.iter().find(|(k, _)| *k == key) {
                        return Lookup::Name(n.to_string());
                    }
                }
                Lookup::Code(canonical)
            }
            NameType::DateTimeField => {
                if !DATE_TIME_FIELDS.contains(&code) {
                    return Lookup::Invalid;
                }
                if let Some(t) = self.table() {
                    if let Some((_, widths)) = t.fields.iter().find(|(k, _)| *k == code) {
                        let w = match self.style {
                            NameStyle::Long => 0,
                            NameStyle::Short => 1,
                            NameStyle::Narrow => 2,
                        };
                        return Lookup::Name(widths[w].to_string());
                    }
                }
                Lookup::Code(code.to_string())
            }
        }
    }

    fn of_language(&self, code: &str) -> Lookup {
        // unicode_language_id: language[-script][-region](-variant)*, no
        // extensions or private use.
        let parsed: Result<icu_locale_core::LanguageIdentifier, _> = code.parse();
        let Ok(id) = parsed else {
            // ICU4X rejects 5-8 letter languages that the grammar allows;
            // a structurally valid bare language id has no name.
            let lower = code.to_ascii_lowercase();
            if crate::locale::is_structurally_valid(code) && !lower.contains("-u-") && !lower.contains("-x-") && !lower.contains("-t-") {
                return Lookup::Code(crate::locale::canonicalize(code).unwrap_or_else(|| code.to_string()));
            }
            return Lookup::Invalid;
        };
        if id.language.is_unknown() && !code.eq_ignore_ascii_case("und") {
            return Lookup::Invalid;
        }
        if code.contains("-u-") || code.contains("-x-") || code.contains("-t-") {
            return Lookup::Invalid;
        }
        // Reject duplicate variants, which parse accepts but the spec does
        // not.
        let mut seen: Vec<String> = Vec::new();
        for v in id.variants.iter() {
            let s = v.to_string();
            if seen.contains(&s) {
                return Lookup::Invalid;
            }
            seen.push(s);
        }
        let canonical = crate::locale::canonicalize(code).unwrap_or_else(|| id.to_string());
        let prefs = self.prefs();
        let opts = self.options();
        if id.script.is_none() && id.region.is_none() && id.variants.is_empty() {
            if let Ok(names) = LanguageDisplayNames::try_new(prefs, opts) {
                if let Some(n) = names.of(id.language) {
                    return Lookup::Name(n.to_string());
                }
            }
            return Lookup::Code(canonical);
        }
        if !id.language.is_unknown() {
            match LanguageDisplayNames::try_new(prefs, opts) {
                Ok(names) if names.of(id.language).is_some() => {}
                // No name for the language itself: the spec falls back to
                // the code rather than "xx (XX)".
                _ => return Lookup::Code(canonical),
            }
        }
        let Ok(formatter) = LocaleDisplayNamesFormatter::try_new(prefs, opts) else { return Lookup::Code(canonical) };
        let locale = Locale::from(id.clone());
        let name = formatter.of(&locale).into_owned();
        if name.is_empty() || name.eq_ignore_ascii_case(&canonical) || name.eq_ignore_ascii_case(code) {
            // ICU4X falls back to the code when even the language has no
            // name; the spec wants the code only when nothing matched.
            return Lookup::Code(canonical);
        }
        if id.language.is_unknown() {
            // "und-Latn": no language name to anchor the pattern.
            if let Ok(scripts) = ScriptDisplayNames::try_new(self.prefs(), self.options()) {
                if let Some(s) = id.script {
                    if let Some(n) = scripts.of(s) {
                        return Lookup::Name(n.to_string());
                    }
                }
            }
            return Lookup::Code(canonical);
        }
        Lookup::Name(name)
    }
}

fn parse_region(code: &str) -> Option<Region> {
    let b = code.as_bytes();
    let ok = (b.len() == 2 && b.iter().all(u8::is_ascii_alphabetic)) || (b.len() == 3 && b.iter().all(u8::is_ascii_digit));
    if !ok {
        return None;
    }
    Region::try_from_str(code).ok()
}

fn parse_script(code: &str) -> Option<Script> {
    let b = code.as_bytes();
    if b.len() != 4 || !b.iter().all(u8::is_ascii_alphabetic) {
        return None;
    }
    Script::try_from_str(code).ok()
}

#[allow(dead_code)]
fn _types(_: Language, _: Variant, _: &VariantDisplayNames) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn dn(tag: &str, ty: NameType, style: NameStyle) -> IntlDisplayNames {
        IntlDisplayNames::new(tag, ty, style, true).unwrap()
    }

    #[test]
    fn names() {
        let l = dn("en", NameType::Language, NameStyle::Long);
        assert_eq!(l.of("fr"), Lookup::Name("French".into()));
        assert_eq!(l.of("en-US"), Lookup::Name("American English".into()));
        assert_eq!(l.of("fr-CA"), Lookup::Name("Canadian French".into()));
        assert!(matches!(l.of("zh-Hant"), Lookup::Name(n) if n.starts_with("Traditional")));
        assert_eq!(l.of("en-u-ca-gregory"), Lookup::Invalid);
        assert_eq!(l.of("en-"), Lookup::Invalid);
        assert_eq!(l.of("xx-XX"), Lookup::Code("xx-XX".into()));
        let std = IntlDisplayNames::new("en", NameType::Language, NameStyle::Long, false).unwrap();
        assert_eq!(std.of("en-US"), Lookup::Name("English (United States)".into()));
        let r = dn("en", NameType::Region, NameStyle::Long);
        assert_eq!(r.of("US"), Lookup::Name("United States".into()));
        assert_eq!(r.of("us"), Lookup::Name("United States".into()));
        assert_eq!(r.of("419"), Lookup::Name("Latin America".into()));
        assert_eq!(r.of("USA"), Lookup::Invalid);
        assert_eq!(r.of("ZZ"), Lookup::Name("Unknown Region".into()));
        let s = dn("en", NameType::Script, NameStyle::Long);
        assert_eq!(s.of("Latn"), Lookup::Name("Latin".into()));
        assert_eq!(s.of("latn"), Lookup::Name("Latin".into()));
        assert_eq!(s.of("Lat"), Lookup::Invalid);
        let c = dn("en", NameType::Currency, NameStyle::Long);
        assert_eq!(c.of("USD"), Lookup::Name("US Dollar".into()));
        assert_eq!(c.of("usd"), Lookup::Name("US Dollar".into()));
        assert_eq!(c.of("US"), Lookup::Invalid);
        assert_eq!(c.of("XXZ"), Lookup::Code("XXZ".into()));
        let cal = dn("en", NameType::Calendar, NameStyle::Long);
        assert_eq!(cal.of("gregory"), Lookup::Name("Gregorian Calendar".into()));
        assert_eq!(cal.of("buddhist"), Lookup::Name("Buddhist Calendar".into()));
        assert_eq!(cal.of("a"), Lookup::Invalid);
        let f = dn("en", NameType::DateTimeField, NameStyle::Long);
        assert_eq!(f.of("weekOfYear"), Lookup::Name("week".into()));
        assert_eq!(f.of("timeZoneName"), Lookup::Name("time zone".into()));
        assert_eq!(f.of("weekOfMonth"), Lookup::Invalid);
        let fs = dn("en", NameType::DateTimeField, NameStyle::Short);
        assert_eq!(fs.of("year"), Lookup::Name("yr.".into()));
        let de = dn("de", NameType::Calendar, NameStyle::Long);
        assert_eq!(de.of("gregory"), Lookup::Name("Gregorianischer Kalender".into()));
        let fr = dn("fr", NameType::Region, NameStyle::Long);
        assert_eq!(fr.of("DE"), Lookup::Name("Allemagne".into()));
    }
}
