//! Enumerations ECMA-402 exposes through `Intl.supportedValuesOf` and
//! `Intl.Locale`'s info getters, answered from ICU4X data.

use std::sync::OnceLock;

use icu_locale_core::Locale;

/// The calendars `Intl.DateTimeFormat` accepts, in canonical form and
/// sorted (AvailableCanonicalCalendars). `islamic` and `islamic-rgsa`
/// are accepted as aliases of `islamic-civil` on input; `islamicc` and
/// `ethiopic-amete-alem` are canonicalised to `islamic-civil` and
/// `ethioaa`.
pub static CALENDARS: &[&str] = &[
    "buddhist",
    "chinese",
    "coptic",
    "dangi",
    "ethioaa",
    "ethiopic",
    "gregory",
    "hebrew",
    "indian",
    "islamic",
    "islamic-civil",
    "islamic-rgsa",
    "islamic-tbla",
    "islamic-umalqura",
    "iso8601",
    "japanese",
    "persian",
    "roc",
];

/// Collation types with ICU4X tailorings or root-equivalent behaviour,
/// excluding `standard` and `search` as ECMA-402 requires.
/// The tailorings ICU4X's default collation data includes: the root
/// `emoji` and `eor` plus the per-language ones in [`collations_for`].
/// (`search`, `searchjl`, `big5han`, `gb2312` and `unihan` are opt-in
/// data that is not baked.)
pub static COLLATIONS: &[&str] = &[
    "compat", "dict", "emoji", "eor", "phonebk", "pinyin", "stroke", "trad", "zhuyin",
];

/// CanonicalizeUValue for `ca`: apply the UTS 35 aliases.
pub fn canonical_calendar(ca: &str) -> String {
    match ca {
        "islamicc" => "islamic-civil".to_string(),
        "ethiopic-amete-alem" => "ethioaa".to_string(),
        "gregorian" => "gregory".to_string(),
        other => other.to_ascii_lowercase(),
    }
}

/// All numbering systems with simple digit mappings, sorted.
pub fn numbering_systems() -> Vec<String> {
    crate::tables::NUMBERING_SYSTEM_DIGITS.iter().map(|(n, _)| n.to_string()).collect()
}

/// The ten digits of a simple numbering system.
pub fn digits_of(nu: &str) -> Option<Vec<char>> {
    crate::tables::NUMBERING_SYSTEM_DIGITS
        .iter()
        .find(|(n, _)| *n == nu)
        .map(|(_, d)| d.chars().collect())
}

pub fn is_numbering_system(nu: &str) -> bool {
    crate::tables::NUMBERING_SYSTEM_DIGITS.iter().any(|(n, _)| *n == nu)
}

/// The sanctioned simple unit identifiers.
pub fn units() -> Vec<String> {
    crate::tables::SANCTIONED_UNITS.iter().map(|s| s.to_string()).collect()
}

pub fn is_sanctioned_unit(u: &str) -> bool {
    crate::tables::SANCTIONED_UNITS.contains(&u)
}

/// IsWellFormedUnitIdentifier: a sanctioned unit or `a-per-b` of two.
pub fn is_well_formed_unit(u: &str) -> bool {
    if is_sanctioned_unit(u) {
        return true;
    }
    if let Some((a, b)) = u.split_once("-per-") {
        return is_sanctioned_unit(a) && is_sanctioned_unit(b);
    }
    false
}

/// IsWellFormedCurrencyCode: three ASCII letters.
pub fn is_well_formed_currency(c: &str) -> bool {
    c.len() == 3 && c.bytes().all(|b| b.is_ascii_alphabetic())
}

/// Currencies CLDR knows (sorted ISO 4217 codes), from ICU4X's currency
/// essentials for the root locale.
pub fn currencies() -> &'static [String] {
    static C: OnceLock<Vec<String>> = OnceLock::new();
    C.get_or_init(|| {
        let mut v: Vec<String> = crate::number::known_currencies();
        v.sort();
        v.dedup();
        v
    })
}

/// Preferred calendars of a locale, most preferred first (CLDR
/// `calendarPreferenceData` by region, through ICU4X).
pub fn calendars_for(tag: &str) -> Vec<String> {
    let loc: Locale = match tag.parse() {
        Ok(l) => l,
        Err(_) => return vec!["gregory".to_string()],
    };
    let mut base = loc.clone();
    base.extensions.unicode.keywords.remove(icu_locale_core::extensions::unicode::key!("ca"));
    let prefs = icu_calendar::preferences::CalendarPreferences::from(&base);
    let first = calendar_algorithm_name(prefs.resolved_algorithm());
    let mut v = vec![first.clone()];
    if first != "gregory" {
        v.push("gregory".to_string());
    }
    v
}

fn calendar_algorithm_name(a: icu_calendar::preferences::CalendarAlgorithm) -> String {
    use icu_locale_core::extensions::unicode::Value;
    let v: Value = a.into();
    canonical_calendar(&v.to_string())
}

/// Collations of a locale: the default first, then its tailorings.
pub fn collations_for(tag: &str) -> Vec<String> {
    let lang = crate::locale::parts(tag).language;
    let tailored: &[&str] = match lang.as_str() {
        "zh" => &["pinyin", "stroke", "zhuyin"],
        "de" => &["phonebk"],
        "es" => &["trad"],
        "ar" => &["compat"],
        // `ln` (phonetic) is not in ICU4X 2.4's baked locale set.
        "si" => &["dict"],
        _ => &[],
    };
    let mut v = vec!["default".to_string()];
    v.extend(tailored.iter().map(|s| s.to_string()));
    v.push("emoji".to_string());
    v.push("eor".to_string());
    v
}

/// Hour cycles of a locale: the preferred one first, then the others.
/// The 12-hour cycle a locale prefers (CLDR timeData: Japan allows `K`
/// before `h`, every other region prefers `h`).
pub fn hour_cycle_12(tag: &str) -> &'static str {
    let max = crate::locale::maximize(&crate::locale::base_name(tag));
    match crate::locale::parts(&max).region.as_deref() {
        Some("JP") => "h11",
        _ => "h12",
    }
}

pub fn hour_cycles_for(tag: &str) -> Vec<String> {
    let first = crate::datetime::default_hour_cycle(tag);
    let mut v = vec![first.clone()];
    for hc in ["h11", "h12", "h23", "h24"] {
        if hc != first {
            v.push(hc.to_string());
        }
    }
    v
}

/// Numbering systems of a locale: the default first, then `latn` if
/// different (`native`, `traditio` and `finance` are resolved in the VM).
pub fn numbering_systems_for(tag: &str) -> Vec<String> {
    let first = crate::number::default_numbering_system(tag);
    let mut v = vec![first.clone()];
    if first != "latn" {
        v.push("latn".to_string());
    }
    v
}

/// Week information: first day (1 = Monday … 7 = Sunday) and weekend days.
pub fn week_info(tag: &str) -> (u8, Vec<u8>) {
    let loc: Locale = match tag.parse() {
        Ok(l) => l,
        Err(_) => return (1, vec![6, 7]),
    };
    let prefs = icu_calendar::week::WeekPreferences::from(&loc);
    match icu_calendar::week::WeekInformation::try_new(prefs) {
        Ok(wi) => {
            let first = wi.first_weekday as u8;
            let mut weekend: Vec<u8> = wi.weekend().map(|d| d as u8).collect();
            weekend.sort();
            (first, weekend)
        }
        Err(_) => (1, vec![6, 7]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_info() {
        assert_eq!(calendars_for("en-US")[0], "gregory");
        assert_eq!(calendars_for("th-TH")[0], "buddhist");
        assert_eq!(week_info("en-US").0, 7);
        assert_eq!(week_info("de-DE").0, 1);
        assert_eq!(week_info("en-US").1, vec![6, 7]);
        assert!(currencies().iter().any(|c| c == "USD"));
        assert!(numbering_systems().len() > 60);
        assert!(is_well_formed_unit("kilometer-per-hour"));
        assert!(!is_well_formed_unit("kilometer-per-hour-per-second"));
    }
}
