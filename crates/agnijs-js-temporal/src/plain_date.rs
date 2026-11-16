//! `Temporal.PlainDate` values.

use std::cmp::Ordering;

use temporal_rs::partial::PartialDate;

use crate::calendar::{Calendar, DateFields, MonthCode};
use crate::duration::{created, Duration};
use crate::instant::settings;
use crate::options::{Overflow, RoundingOptions, ShowCalendar};
use crate::{guard, Result};

/// A calendar date ([[InitializedTemporalDate]]): an ISO date within the
/// representable range and a calendar.
#[derive(Debug, Clone, PartialEq)]
pub struct PlainDate(pub(crate) temporal_rs::PlainDate);

impl PlainDate {
    /// CreateTemporalDate from ISO fields: a `RangeError` unless they are
    /// a valid ISO date within the limits (IsValidISODate,
    /// ISODateWithinLimits).
    pub fn new(iso_year: i32, iso_month: u8, iso_day: u8, calendar: Calendar) -> Result<PlainDate> {
        guard(|| Ok(PlainDate(temporal_rs::PlainDate::try_new(iso_year, iso_month, iso_day, calendar.0)?)))
    }

    /// CalendarDateFromFields: a `TypeError` when a field the calendar
    /// needs is missing, a `RangeError` when the fields name no date (with
    /// `Reject`) or one out of range.
    pub fn from_fields(fields: &DateFields, calendar: Calendar, overflow: Overflow) -> Result<PlainDate> {
        guard(|| {
            let partial = PartialDate { calendar_fields: fields.to_rs()?, calendar: calendar.0 };
            Ok(PlainDate(temporal_rs::PlainDate::from_partial(partial, Some(overflow.to_rs()))?))
        })
    }

    /// ParseTemporalDateString followed by CreateTemporalDate: an ISO
    /// 8601 date or date-time without a `Z` designator.
    pub fn parse(s: &str) -> Result<PlainDate> {
        guard(|| Ok(PlainDate(temporal_rs::PlainDate::from_utf8(s.as_bytes())?)))
    }

    /// The fields of `self` merged with `fields` (CalendarMergeFields),
    /// then resolved as by [`PlainDate::from_fields`].
    pub fn with(&self, fields: &DateFields, overflow: Overflow) -> Result<PlainDate> {
        guard(|| Ok(PlainDate(self.0.with(fields.to_rs()?, Some(overflow.to_rs()))?)))
    }

    pub fn with_calendar(&self, calendar: Calendar) -> PlainDate {
        PlainDate(self.0.with_calendar(calendar.0))
    }

    pub fn calendar(&self) -> Calendar {
        Calendar(self.0.calendar().clone())
    }

    /// CompareISODate: the calendars play no part.
    pub fn compare(&self, other: &PlainDate) -> Ordering {
        self.0.compare_iso(&other.0)
    }

    /// AddDurationToDate: the duration's time units balance into days.
    pub fn add(&self, d: &Duration, overflow: Overflow) -> Result<PlainDate> {
        guard(|| Ok(PlainDate(self.0.add(&d.0, Some(overflow.to_rs()))?)))
    }

    pub fn subtract(&self, d: &Duration, overflow: Overflow) -> Result<PlainDate> {
        guard(|| Ok(PlainDate(self.0.subtract(&d.0, Some(overflow.to_rs()))?)))
    }

    /// DifferenceTemporalPlainDate(until) after the calendars are checked
    /// and the settings read.
    pub fn until(&self, other: &PlainDate, options: RoundingOptions) -> Result<Duration> {
        guard(|| Ok(created(self.0.until(&other.0, settings(options)?)?)))
    }

    pub fn since(&self, other: &PlainDate, options: RoundingOptions) -> Result<Duration> {
        guard(|| Ok(created(self.0.since(&other.0, settings(options)?)?)))
    }

    /// TemporalDateToString.
    pub fn to_string_with(&self, show_calendar: ShowCalendar) -> Result<String> {
        guard(|| Ok(self.0.to_ixdtf_string(show_calendar.to_rs())))
    }

    pub fn year(&self) -> i32 {
        self.0.year()
    }
    pub fn month(&self) -> u8 {
        self.0.month()
    }
    pub fn month_code(&self) -> MonthCode {
        MonthCode(self.0.month_code())
    }
    pub fn day(&self) -> u8 {
        self.0.day()
    }
    pub fn day_of_week(&self) -> u16 {
        self.0.day_of_week()
    }
    pub fn day_of_year(&self) -> u16 {
        self.0.day_of_year()
    }
    /// `None` for a calendar without numbered weeks.
    pub fn week_of_year(&self) -> Option<u8> {
        self.0.week_of_year()
    }
    pub fn year_of_week(&self) -> Option<i32> {
        self.0.year_of_week()
    }
    pub fn days_in_week(&self) -> u16 {
        self.0.days_in_week()
    }
    pub fn days_in_month(&self) -> u16 {
        self.0.days_in_month()
    }
    pub fn days_in_year(&self) -> u16 {
        self.0.days_in_year()
    }
    pub fn months_in_year(&self) -> u16 {
        self.0.months_in_year()
    }
    pub fn in_leap_year(&self) -> bool {
        self.0.in_leap_year()
    }
    /// `None` for a calendar without eras.
    pub fn era(&self) -> Option<String> {
        self.0.era().map(|e| e.to_string())
    }
    pub fn era_year(&self) -> Option<i32> {
        self.0.era_year()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::Unit;
    use crate::ErrorKind;

    fn date(s: &str) -> PlainDate {
        PlainDate::parse(s).unwrap()
    }

    #[test]
    fn dates_are_created_checked_and_printed() {
        let d = PlainDate::new(2024, 2, 29, Calendar::iso()).unwrap();
        assert_eq!(d.to_string_with(ShowCalendar::Auto).unwrap(), "2024-02-29");
        assert_eq!(d.to_string_with(ShowCalendar::Always).unwrap(), "2024-02-29[u-ca=iso8601]");
        assert_eq!(PlainDate::new(2023, 2, 29, Calendar::iso()).unwrap_err().kind, ErrorKind::Range);
        assert_eq!(PlainDate::new(-271821, 4, 18, Calendar::iso()).unwrap_err().kind, ErrorKind::Range);
        assert!(PlainDate::new(-271821, 4, 19, Calendar::iso()).is_ok());
        assert_eq!(PlainDate::parse("2020-01-01T00:00Z").unwrap_err().kind, ErrorKind::Range);
        assert_eq!(date("2020-01-01[u-ca=gregory]").calendar().identifier(), "gregory");
    }

    #[test]
    fn fields_resolve_with_overflow_and_missing_fields_are_type_errors() {
        let mut f = DateFields { year: Some(2023), month: Some(2), day: Some(31), ..Default::default() };
        let constrained = PlainDate::from_fields(&f, Calendar::iso(), Overflow::Constrain).unwrap();
        assert_eq!(constrained.day(), 28);
        assert_eq!(PlainDate::from_fields(&f, Calendar::iso(), Overflow::Reject).unwrap_err().kind, ErrorKind::Range);
        f.day = None;
        assert_eq!(PlainDate::from_fields(&f, Calendar::iso(), Overflow::Constrain).unwrap_err().kind, ErrorKind::Type);
        let mc = DateFields { day: Some(5), month_code: Some(MonthCode::parse("M03").unwrap()), ..Default::default() };
        assert_eq!(constrained.with(&mc, Overflow::Constrain).unwrap(), date("2023-03-05"));
    }

    #[test]
    fn arithmetic_differences_and_calendar_getters() {
        let d = date("2024-01-31");
        let month = Duration::parse("P1M").unwrap();
        assert_eq!(d.add(&month, Overflow::Constrain).unwrap(), date("2024-02-29"));
        assert_eq!(d.add(&month, Overflow::Reject).unwrap_err().kind, ErrorKind::Range);
        let months = RoundingOptions { largest_unit: Some(Unit::Month), ..Default::default() };
        let diff = d.until(&date("2024-03-15"), months).unwrap();
        assert_eq!(diff.to_string_with(Default::default()).unwrap(), "P1M15D");
        assert_eq!(d.compare(&date("2024-02-01")), Ordering::Less);
        assert_eq!((d.day_of_week(), d.day_of_year(), d.week_of_year(), d.days_in_year()), (3, 31, Some(5), 366));
        let japanese = d.with_calendar(Calendar::from_identifier("japanese").unwrap());
        assert_eq!((japanese.era().as_deref(), japanese.era_year()), (Some("reiwa"), Some(6)));
        assert_eq!(d.era(), None);
    }
}
