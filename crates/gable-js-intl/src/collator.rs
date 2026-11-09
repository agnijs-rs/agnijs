//! `Intl.Collator` on ICU4X's collator.

use std::cmp::Ordering;

use icu_collator::options::{AlternateHandling, CaseLevel, CollatorOptions, MaxVariable, Strength};
use icu_collator::preferences::{CollationCaseFirst, CollationNumericOrdering};
use icu_collator::{Collator, CollatorBorrowed, CollatorPreferences};
use icu_locale_core::Locale;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sensitivity {
    Base,
    Accent,
    Case,
    Variant,
}

impl Sensitivity {
    pub fn parse(s: &str) -> Option<Sensitivity> {
        Some(match s {
            "base" => Sensitivity::Base,
            "accent" => Sensitivity::Accent,
            "case" => Sensitivity::Case,
            "variant" => Sensitivity::Variant,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Sensitivity::Base => "base",
            Sensitivity::Accent => "accent",
            Sensitivity::Case => "case",
            Sensitivity::Variant => "variant",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollatorOptionsIn {
    /// The resolved locale, possibly with `-u-co`, `-u-kn`, `-u-kf`.
    pub locale: String,
    pub search: bool,
    pub sensitivity: Sensitivity,
    /// `None` keeps the locale's own default (Thai ignores punctuation).
    pub ignore_punctuation: Option<bool>,
    pub numeric: bool,
    /// `upper`, `lower` or `false`.
    pub case_first: String,
}

pub struct IntlCollator {
    collator: CollatorBorrowed<'static>,
}

impl IntlCollator {
    pub fn new(o: &CollatorOptionsIn) -> Result<IntlCollator, String> {
        let locale: Locale = o.locale.parse().map_err(|_| format!("bad locale {}", o.locale))?;
        let mut prefs = CollatorPreferences::from(&locale);
        prefs.numeric_ordering = Some(if o.numeric { CollationNumericOrdering::True } else { CollationNumericOrdering::False });
        prefs.case_first = Some(match o.case_first.as_str() {
            "upper" => CollationCaseFirst::Upper,
            "lower" => CollationCaseFirst::Lower,
            _ => CollationCaseFirst::False,
        });
        let mut opts = CollatorOptions::default();
        opts.strength = Some(match o.sensitivity {
            Sensitivity::Base => Strength::Primary,
            Sensitivity::Accent => Strength::Secondary,
            Sensitivity::Case => Strength::Primary,
            Sensitivity::Variant => Strength::Tertiary,
        });
        opts.case_level = Some(if o.sensitivity == Sensitivity::Case { CaseLevel::On } else { CaseLevel::Off });
        match o.ignore_punctuation {
            Some(true) => {
                opts.alternate_handling = Some(AlternateHandling::Shifted);
                opts.max_variable = Some(MaxVariable::Punctuation);
            }
            Some(false) => opts.alternate_handling = Some(AlternateHandling::NonIgnorable),
            None => {}
        }
        let collator = Collator::try_new(prefs, opts).map_err(|e| e.to_string())?;
        Ok(IntlCollator { collator })
    }

    pub fn compare(&self, a: &[u16], b: &[u16]) -> Ordering {
        self.collator.compare_utf16(a, b)
    }

    /// Whether the collator ended up with punctuation ignored (the locale
    /// may default to shifted handling, as Thai does).
    pub fn ignores_punctuation(&self) -> bool {
        self.collator.resolved_options().alternate_handling == AlternateHandling::Shifted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn c(locale: &str, sens: Sensitivity, numeric: bool, punct: bool) -> IntlCollator {
        IntlCollator::new(&CollatorOptionsIn {
            locale: locale.into(),
            search: false,
            sensitivity: sens,
            ignore_punctuation: Some(punct),
            numeric,
            case_first: "false".into(),
        })
        .unwrap()
    }

    #[test]
    fn sensitivities() {
        let base = c("en", Sensitivity::Base, false, false);
        assert_eq!(base.compare(&u("a"), &u("A")), Ordering::Equal);
        assert_eq!(base.compare(&u("a"), &u("á")), Ordering::Equal);
        let accent = c("en", Sensitivity::Accent, false, false);
        assert_eq!(accent.compare(&u("a"), &u("A")), Ordering::Equal);
        assert_ne!(accent.compare(&u("a"), &u("á")), Ordering::Equal);
        let case = c("en", Sensitivity::Case, false, false);
        assert_ne!(case.compare(&u("a"), &u("A")), Ordering::Equal);
        assert_eq!(case.compare(&u("a"), &u("á")), Ordering::Equal);
        let variant = c("en", Sensitivity::Variant, false, false);
        assert_eq!(variant.compare(&u("a"), &u("b")), Ordering::Less);
        assert_eq!(variant.compare(&u("ä"), &u("z")), Ordering::Less);
        let sv = c("sv", Sensitivity::Variant, false, false);
        assert_eq!(sv.compare(&u("ä"), &u("z")), Ordering::Greater);
        let num = c("en", Sensitivity::Variant, true, false);
        assert_eq!(num.compare(&u("2"), &u("10")), Ordering::Less);
        let punct = c("en", Sensitivity::Variant, false, true);
        assert_eq!(punct.compare(&u("a-b"), &u("ab")), Ordering::Equal);
    }
}
