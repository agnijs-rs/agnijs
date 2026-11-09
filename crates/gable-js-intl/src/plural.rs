//! `Intl.PluralRules`: cardinal and ordinal categories of values after
//! ECMA-402 rounding, and the category of a range.

use icu_locale_core::Locale;
use icu_plurals::{PluralCategory, PluralRules, PluralRulesPreferences, PluralRulesWithRanges};

use crate::decimal::Dec;
use crate::number::{
    category_name, operands, operands_with_exponent, round_with, Digits, Grouping, Notation, Num, NumberFormatOptions,
    NumberFormatter, SignDisplay, Style,
};

pub struct PluralSelector {
    rules: PluralRulesWithRanges<PluralRules>,
    digits: Digits,
    /// Set for the non-standard notations: the value is formatted first and
    /// the significand and exponent select the category.
    formatter: Option<NumberFormatter>,
}

impl PluralSelector {
    /// `notation` is `standard`, `compact`, `scientific` or `engineering`;
    /// `compact_long` selects the long compact affixes.
    pub fn new(tag: &str, ordinal: bool, digits: Digits, notation: &str, compact_long: bool) -> Result<PluralSelector, String> {
        let seam_notation = match notation {
            "compact" => Some(Notation::Compact { long: compact_long }),
            "scientific" => Some(Notation::Scientific),
            "engineering" => Some(Notation::Engineering),
            _ => None,
        };
        let formatter = match seam_notation {
            Some(n) => Some(NumberFormatter::new(NumberFormatOptions {
                locale: tag.to_string(),
                numbering_system: "latn".to_string(),
                style: Style::Decimal,
                notation: n,
                grouping: Grouping::Auto,
                sign_display: SignDisplay::Auto,
                digits: digits.clone(),
            })?),
            None => None,
        };
        let base = crate::locale::base_name(tag);
        let locale: Locale = base.parse().map_err(|_| format!("bad locale {tag}"))?;
        let prefs = PluralRulesPreferences::from(&locale);
        let rules = if ordinal {
            PluralRulesWithRanges::try_new_ordinal(prefs)
        } else {
            PluralRulesWithRanges::try_new_cardinal(prefs)
        }
        .map_err(|e| e.to_string())?;
        Ok(PluralSelector { rules, digits, formatter })
    }

    fn operands_of(&self, x: &Dec) -> icu_plurals::PluralOperands {
        if let Some(f) = &self.formatter {
            if let (_, Some(r)) = f.format_inner(&Num::Finite(x.clone())) {
                return operands_with_exponent(&r.value, r.exponent);
            }
        }
        operands(&round_with(&self.digits, x))
    }

    /// ResolvePlural: the category of a finite value after rounding (and
    /// notation).
    pub fn select(&self, x: &Dec) -> &'static str {
        category_name(self.rules.rules().category_for(self.operands_of(x)))
    }

    /// ResolvePluralRange.
    pub fn select_range(&self, a: &Dec, b: &Dec) -> &'static str {
        category_name(self.rules.category_for_range(self.operands_of(a), self.operands_of(b)))
    }

    /// The categories the rules can produce, in `zero one two few many
    /// other` order.
    pub fn categories(&self) -> Vec<&'static str> {
        let mut v: Vec<PluralCategory> = self.rules.rules().categories().collect();
        v.sort_by_key(|c| match c {
            PluralCategory::Zero => 0,
            PluralCategory::One => 1,
            PluralCategory::Two => 2,
            PluralCategory::Few => 3,
            PluralCategory::Many => 4,
            PluralCategory::Other => 5,
        });
        v.into_iter().map(category_name).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_cardinal_and_ordinal() {
        let c = PluralSelector::new("en", false, Digits::decimal(), "standard", false).unwrap();
        assert_eq!(c.select(&Dec::from_f64(1.0)), "one");
        assert_eq!(c.select(&Dec::from_f64(2.0)), "other");
        let mut d = Digits::decimal();
        d.min_fraction = 1;
        let c1 = PluralSelector::new("en", false, d, "standard", false).unwrap();
        assert_eq!(c1.select(&Dec::from_f64(1.0)), "other");
        let o = PluralSelector::new("en", true, Digits::decimal(), "standard", false).unwrap();
        assert_eq!(o.select(&Dec::from_f64(1.0)), "one");
        assert_eq!(o.select(&Dec::from_f64(2.0)), "two");
        assert_eq!(o.select(&Dec::from_f64(3.0)), "few");
        assert_eq!(o.select(&Dec::from_f64(11.0)), "other");
        assert_eq!(o.categories(), vec!["one", "two", "few", "other"]);
        assert_eq!(c.select_range(&Dec::from_f64(1.0), &Dec::from_f64(2.0)), "other");
        let fr = PluralSelector::new("fr", false, Digits::decimal(), "standard", false).unwrap();
        assert_eq!(fr.select_range(&Dec::from_f64(0.0), &Dec::from_f64(1.0)), "one");
        assert_eq!(fr.select(&Dec::from_f64(1.5e6)), "other");
        let frc = PluralSelector::new("fr", false, Digits::decimal(), "compact", false).unwrap();
        assert_eq!(frc.select(&Dec::from_f64(1.5e6)), "many");
        assert_eq!(frc.select(&Dec::from_f64(1e6)), "many");
        assert_eq!(frc.select(&Dec::from_f64(1e-6)), "one");
    }
}
