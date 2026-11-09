//! `Intl.RelativeTimeFormat` on ICU4X's experimental relative time
//! formatter, with numeric parts from the number formatter.

use fixed_decimal::Decimal;
use icu_experimental::relativetime::options::{Numeric, RelativeTimeFormatterOptions};
use icu_experimental::relativetime::{RelativeTimeFormatter, RelativeTimeFormatterPreferences};
use icu_locale_core::Locale;
use writeable::{PartsWrite, Writeable};

use crate::number::{Digits, Grouping, Notation, Num, NumberFormatOptions, NumberFormatter, SignDisplay, Style};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelUnit {
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

impl RelUnit {
    /// Accepts singular and plural unit names.
    pub fn parse(s: &str) -> Option<RelUnit> {
        Some(match s {
            "second" | "seconds" => RelUnit::Second,
            "minute" | "minutes" => RelUnit::Minute,
            "hour" | "hours" => RelUnit::Hour,
            "day" | "days" => RelUnit::Day,
            "week" | "weeks" => RelUnit::Week,
            "month" | "months" => RelUnit::Month,
            "quarter" | "quarters" => RelUnit::Quarter,
            "year" | "years" => RelUnit::Year,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            RelUnit::Second => "second",
            RelUnit::Minute => "minute",
            RelUnit::Hour => "hour",
            RelUnit::Day => "day",
            RelUnit::Week => "week",
            RelUnit::Month => "month",
            RelUnit::Quarter => "quarter",
            RelUnit::Year => "year",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelStyle {
    Long,
    Short,
    Narrow,
}

pub struct IntlRelativeTimeFormatter {
    locale: String,
    numbering_system: String,
    style: RelStyle,
    numeric_auto: bool,
    numbers: NumberFormatter,
}

/// A part of relative-time output: `literal`, or a number part carrying
/// the unit name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelPart {
    pub kind: &'static str,
    pub value: String,
    pub unit: Option<&'static str>,
}

impl IntlRelativeTimeFormatter {
    pub fn new(locale: &str, numbering_system: &str, style: RelStyle, numeric_auto: bool) -> Result<IntlRelativeTimeFormatter, String> {
        let numbers = NumberFormatter::new(NumberFormatOptions {
            locale: locale.to_string(),
            numbering_system: numbering_system.to_string(),
            style: Style::Decimal,
            notation: Notation::Standard,
            grouping: Grouping::Auto,
            sign_display: SignDisplay::Auto,
            digits: Digits::decimal(),
        })?;
        Ok(IntlRelativeTimeFormatter {
            locale: locale.to_string(),
            numbering_system: numbering_system.to_string(),
            style,
            numeric_auto,
            numbers,
        })
    }

    fn icu(&self, unit: RelUnit) -> Result<RelativeTimeFormatter, String> {
        let mut tag = crate::locale::base_name(&self.locale);
        if !self.numbering_system.is_empty() {
            tag = format!("{tag}-u-nu-{}", self.numbering_system);
        }
        let locale: Locale = tag.parse().map_err(|_| format!("bad locale {tag}"))?;
        let prefs = RelativeTimeFormatterPreferences::from(&locale);
        let mut opts = RelativeTimeFormatterOptions::default();
        opts.numeric = Some(if self.numeric_auto { Numeric::Auto } else { Numeric::Always });
        use RelStyle::*;
        use RelUnit::*;
        let r = match (self.style, unit) {
            (Long, Second) => RelativeTimeFormatter::try_new_long_second(prefs, opts),
            (Long, Minute) => RelativeTimeFormatter::try_new_long_minute(prefs, opts),
            (Long, Hour) => RelativeTimeFormatter::try_new_long_hour(prefs, opts),
            (Long, Day) => RelativeTimeFormatter::try_new_long_day(prefs, opts),
            (Long, Week) => RelativeTimeFormatter::try_new_long_week(prefs, opts),
            (Long, Month) => RelativeTimeFormatter::try_new_long_month(prefs, opts),
            (Long, Quarter) => RelativeTimeFormatter::try_new_long_quarter(prefs, opts),
            (Long, Year) => RelativeTimeFormatter::try_new_long_year(prefs, opts),
            (Short, Second) => RelativeTimeFormatter::try_new_short_second(prefs, opts),
            (Short, Minute) => RelativeTimeFormatter::try_new_short_minute(prefs, opts),
            (Short, Hour) => RelativeTimeFormatter::try_new_short_hour(prefs, opts),
            (Short, Day) => RelativeTimeFormatter::try_new_short_day(prefs, opts),
            (Short, Week) => RelativeTimeFormatter::try_new_short_week(prefs, opts),
            (Short, Month) => RelativeTimeFormatter::try_new_short_month(prefs, opts),
            (Short, Quarter) => RelativeTimeFormatter::try_new_short_quarter(prefs, opts),
            (Short, Year) => RelativeTimeFormatter::try_new_short_year(prefs, opts),
            (Narrow, Second) => RelativeTimeFormatter::try_new_narrow_second(prefs, opts),
            (Narrow, Minute) => RelativeTimeFormatter::try_new_narrow_minute(prefs, opts),
            (Narrow, Hour) => RelativeTimeFormatter::try_new_narrow_hour(prefs, opts),
            (Narrow, Day) => RelativeTimeFormatter::try_new_narrow_day(prefs, opts),
            (Narrow, Week) => RelativeTimeFormatter::try_new_narrow_week(prefs, opts),
            (Narrow, Month) => RelativeTimeFormatter::try_new_narrow_month(prefs, opts),
            (Narrow, Quarter) => RelativeTimeFormatter::try_new_narrow_quarter(prefs, opts),
            (Narrow, Year) => RelativeTimeFormatter::try_new_narrow_year(prefs, opts),
        };
        r.map_err(|e| e.to_string())
    }

    /// PartitionRelativeTimePattern for a finite value.
    pub fn format_to_parts(&self, value: f64, unit: RelUnit) -> Result<Vec<RelPart>, String> {
        let f = self.icu(unit)?;
        // Negative zero counts as negative ("0 seconds ago" rather than
        // "in 0 seconds"); ICU4X's Decimal keeps the sign.
        // With numeric "auto" the zero phrase ("now", "today") covers -0 too.
        let neg = value.is_sign_negative() && !(self.numeric_auto && value == 0.0);
        let rounded = self.numbers.round(&crate::decimal::Dec::from_f64(value.abs()));
        let mut icu_dec: Decimal = rounded.to_plain(1, 0).parse().map_err(|_| "bad number".to_string())?;
        icu_dec.set_sign(if neg { fixed_decimal::Sign::Negative } else { fixed_decimal::Sign::None });
        let formatted = f.format(icu_dec);
        let mut sink = Sink { runs: Vec::new(), depth: 0, literal: false };
        let _ = formatted.write_to_parts(&mut sink);
        let num_parts = self.numbers.format_to_parts(&Num::from_f64(value.abs()));
        let mut out = Vec::new();
        for (is_literal, text) in sink.runs {
            if is_literal {
                push_literal(&mut out, &text);
            } else {
                for p in &num_parts {
                    out.push(RelPart { kind: p.kind, value: p.value.clone(), unit: Some(unit.name()) });
                }
            }
        }
        if out.is_empty() {
            out.push(RelPart { kind: "literal", value: formatted.write_to_string().into_owned(), unit: None });
        }
        Ok(out)
    }

    pub fn format(&self, value: f64, unit: RelUnit) -> Result<String, String> {
        Ok(self.format_to_parts(value, unit)?.into_iter().map(|p| p.value).collect())
    }
}

fn push_literal(out: &mut Vec<RelPart>, s: &str) {
    if s.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut() {
        if last.kind == "literal" && last.unit.is_none() {
            last.value.push_str(s);
            return;
        }
    }
    out.push(RelPart { kind: "literal", value: s.to_string(), unit: None });
}

/// Collects `(is_literal, text)` runs; text inside a non-`literal` part is
/// the number.
struct Sink {
    runs: Vec<(bool, String)>,
    depth: usize,
    literal: bool,
}

impl Sink {
    fn push(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        // Pattern literals are written with no part at all; the number comes
        // wrapped in decimal parts; numeric-auto phrases come as LITERAL.
        let lit = self.depth == 0 || self.literal;
        if let Some(last) = self.runs.last_mut() {
            if last.0 == lit {
                last.1.push_str(s);
                return;
            }
        }
        self.runs.push((lit, s.to_string()));
    }
}

impl std::fmt::Write for Sink {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.push(s);
        Ok(())
    }
    fn write_char(&mut self, c: char) -> std::fmt::Result {
        let mut b = [0u8; 4];
        self.push(c.encode_utf8(&mut b));
        Ok(())
    }
}

impl PartsWrite for Sink {
    type SubPartsWrite = Sink;
    fn with_part(&mut self, part: writeable::Part, f: impl FnOnce(&mut Sink) -> std::fmt::Result) -> std::fmt::Result {
        let saved = self.literal;
        if self.depth == 0 {
            self.literal = part == icu_experimental::relativetime::parts::LITERAL;
        }
        self.depth += 1;
        let r = f(self);
        self.depth -= 1;
        self.literal = saved;
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english() {
        let f = IntlRelativeTimeFormatter::new("en", "", RelStyle::Long, false).unwrap();
        assert_eq!(f.format(3.0, RelUnit::Day).unwrap(), "in 3 days");
        assert_eq!(f.format(-1.0, RelUnit::Day).unwrap(), "1 day ago");
        assert_eq!(f.format(-0.0, RelUnit::Second).unwrap(), "0 seconds ago");
        assert_eq!(f.format(0.0, RelUnit::Second).unwrap(), "in 0 seconds");
        assert_eq!(f.format(1000.0, RelUnit::Year).unwrap(), "in 1,000 years");
        let parts = f.format_to_parts(-1000.5, RelUnit::Year).unwrap();
        assert_eq!(parts[0].kind, "integer");
        assert_eq!(parts[0].value, "1");
        assert_eq!(parts[0].unit, Some("year"));
        assert_eq!(parts[1].kind, "group");
        assert_eq!(parts.last().unwrap().value, " years ago");
        let a = IntlRelativeTimeFormatter::new("en", "", RelStyle::Long, true).unwrap();
        assert_eq!(a.format(-1.0, RelUnit::Day).unwrap(), "yesterday");
        assert_eq!(a.format(0.0, RelUnit::Day).unwrap(), "today");
        assert_eq!(a.format(2.0, RelUnit::Day).unwrap(), "in 2 days");
        let s = IntlRelativeTimeFormatter::new("en", "", RelStyle::Short, false).unwrap();
        assert_eq!(s.format(3.0, RelUnit::Month).unwrap(), "in 3 mo.");
        let ar = IntlRelativeTimeFormatter::new("ar", "arab", RelStyle::Long, false).unwrap();
        assert!(ar.format(3.0, RelUnit::Day).unwrap().contains('٣'));
    }
}
