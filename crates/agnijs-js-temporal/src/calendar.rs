//! Calendars and the date fields of a property bag.

use crate::{guard, Error, Result};

/// A calendar the library supports, by its canonical identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Calendar(pub(crate) temporal_rs::Calendar);

impl Default for Calendar {
    fn default() -> Calendar {
        Calendar::iso()
    }
}

impl Calendar {
    /// The ISO 8601 calendar, `"iso8601"`.
    pub fn iso() -> Calendar {
        Calendar(temporal_rs::Calendar::ISO)
    }

    /// CanonicalizeCalendar: a calendar identifier in any ASCII case
    /// (`"Gregory"`). An unknown identifier is a `RangeError`.
    pub fn from_identifier(id: &str) -> Result<Calendar> {
        guard(|| Ok(Calendar(temporal_rs::Calendar::try_from_utf8(id.as_bytes())?)))
    }

    /// ToTemporalCalendarIdentifier for a string: a calendar identifier,
    /// or an ISO 8601 string whose annotation names the calendar (none
    /// names the ISO calendar). Anything else is a `RangeError`.
    pub fn parse(s: &str) -> Result<Calendar> {
        guard(|| Ok(Calendar(s.parse::<temporal_rs::Calendar>()?)))
    }

    /// The canonical identifier (`"gregory"`, `"iso8601"`).
    pub fn identifier(&self) -> &'static str {
        self.0.identifier()
    }

    /// Whether dates in the calendar have an era and an era year, so a
    /// property bag's `era` and `eraYear` are read (CalendarSupportsEra).
    pub fn has_eras(&self) -> bool {
        !matches!(self.identifier(), "iso8601" | "chinese" | "dangi")
    }
}

/// A month code (`"M01"`, `"M05L"`), checked for syntax only: whether
/// the month exists depends on the calendar and the year.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MonthCode(pub(crate) temporal_rs::MonthCode);

impl MonthCode {
    /// ToMonthCode's syntax check: a `RangeError` for anything that is
    /// not `M` and two digits, optionally followed by `L`.
    pub fn parse(s: &str) -> Result<MonthCode> {
        guard(|| Ok(MonthCode(temporal_rs::MonthCode::try_from_utf8(s.as_bytes())?)))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// The date fields of a property bag after their coercions
/// (PrepareCalendarFields); `None` is a field the bag left undefined. The
/// engine converts numbers to these types saturating, so an out-of-range
/// value stays out of range and fails as a `RangeError` when the date is
/// resolved.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DateFields {
    pub year: Option<i32>,
    pub month: Option<u8>,
    pub month_code: Option<MonthCode>,
    pub day: Option<u8>,
    pub era: Option<String>,
    pub era_year: Option<i32>,
}

impl DateFields {
    pub fn is_empty(&self) -> bool {
        *self == DateFields::default()
    }

    pub(crate) fn to_rs(&self) -> Result<temporal_rs::fields::CalendarFields> {
        let era = match &self.era {
            None => None,
            Some(e) => Some(
                temporal_rs::TinyAsciiStr::<19>::try_from_str(e)
                    .map_err(|_| Error::range(format!("{e} is not a valid era")))?,
            ),
        };
        let mut f = temporal_rs::fields::CalendarFields::new();
        f.year = self.year;
        f.month = self.month;
        f.month_code = self.month_code.map(|m| m.0);
        f.day = self.day;
        f.era = era;
        f.era_year = self.era_year;
        Ok(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;

    #[test]
    fn calendars_are_canonicalized_and_parsed() {
        assert_eq!(Calendar::from_identifier("Gregory").unwrap().identifier(), "gregory");
        assert_eq!(Calendar::from_identifier("2020-01-01").unwrap_err().kind, ErrorKind::Range);
        assert_eq!(Calendar::parse("2020-01-01").unwrap().identifier(), "iso8601");
        assert_eq!(Calendar::parse("2020-01-01[u-ca=hebrew]").unwrap().identifier(), "hebrew");
        assert_eq!(Calendar::parse("klingon").unwrap_err().kind, ErrorKind::Range);
        assert!(Calendar::from_identifier("japanese").unwrap().has_eras());
        assert!(!Calendar::iso().has_eras() && !Calendar::from_identifier("chinese").unwrap().has_eras());
    }

    #[test]
    fn month_codes_are_checked_for_syntax() {
        assert_eq!(MonthCode::parse("M05L").unwrap().as_str(), "M05L");
        for bad in ["M5", "m05", "M05X", "M123", ""] {
            assert_eq!(MonthCode::parse(bad).unwrap_err().kind, ErrorKind::Range, "{bad}");
        }
    }
}
