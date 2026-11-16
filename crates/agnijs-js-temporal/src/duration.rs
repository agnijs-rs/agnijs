//! `Temporal.Duration` values and the `relativeTo` option.

use std::cmp::Ordering;

use temporal_rs::provider::COMPILED_TZ_PROVIDER;

use crate::options::{RoundingOptions, ToStringOptions, Unit};
use crate::{guard, Result};

/// The ten fields of a duration. Each is the mathematical value the
/// engine read; years, months and weeks must fit in 32 bits and the time
/// fields in 2⁵³ seconds together (IsValidDuration), so `i64` and, for the
/// sub-millisecond fields, `i128` hold every valid duration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DurationFields {
    pub years: i64,
    pub months: i64,
    pub weeks: i64,
    pub days: i64,
    pub hours: i64,
    pub minutes: i64,
    pub seconds: i64,
    pub milliseconds: i64,
    pub microseconds: i128,
    pub nanoseconds: i128,
}

/// A valid duration ([[InitializedTemporalDuration]]).
#[derive(Debug, Clone, PartialEq)]
pub struct Duration(pub(crate) temporal_rs::Duration);

/// The duration CreateTemporalDuration makes from an operation's result:
/// each field is stored as ℝ(𝔽(value)), so a field beyond 2⁵³ loses the
/// precision a Number cannot hold, as the specification requires
/// (`PT18014398509.481981S` is stored as `PT18014398509.48198S`). The
/// library keeps exact integers; this rounds them. Rounding cannot make a
/// valid duration invalid by more than the specification's own
/// conversion does, and if it would, the exact result is kept.
pub(crate) fn created(d: temporal_rs::Duration) -> Duration {
    fn f(v: i64) -> i64 {
        v as f64 as i64
    }
    fn g(v: i128) -> i128 {
        v as f64 as i128
    }
    temporal_rs::Duration::new(
        f(d.years()),
        f(d.months()),
        f(d.weeks()),
        f(d.days()),
        f(d.hours()),
        f(d.minutes()),
        f(d.seconds()),
        f(d.milliseconds()),
        g(d.microseconds()),
        g(d.nanoseconds()),
    )
    .map_or(Duration(d), Duration)
}

/// The `relativeTo` option of `compare`, `round` and `total`: a plain date
/// or a zoned date-time that calendar units are measured from.
#[derive(Debug, Clone)]
pub struct RelativeTo(pub(crate) temporal_rs::options::RelativeTo);

impl RelativeTo {
    /// A `relativeTo` string: an ISO 8601 date or date-time, with a time
    /// zone annotation for a zoned date-time (GetTemporalRelativeToOption
    /// for a String).
    pub fn parse(s: &str) -> Result<RelativeTo> {
        guard(|| {
            Ok(RelativeTo(temporal_rs::options::RelativeTo::try_from_str_with_provider(
                s,
                &*COMPILED_TZ_PROVIDER,
            )?))
        })
    }
}

impl Duration {
    /// CreateTemporalDuration: a `RangeError` unless the fields are a
    /// valid duration (IsValidDuration).
    pub fn new(f: DurationFields) -> Result<Duration> {
        guard(|| {
            Ok(Duration(temporal_rs::Duration::new(
                f.years,
                f.months,
                f.weeks,
                f.days,
                f.hours,
                f.minutes,
                f.seconds,
                f.milliseconds,
                f.microseconds,
                f.nanoseconds,
            )?))
        })
    }

    /// An ISO 8601 duration string (ParseTemporalDurationString).
    pub fn parse(s: &str) -> Result<Duration> {
        guard(|| Ok(created(temporal_rs::Duration::from_utf8(s.as_bytes())?)))
    }

    pub fn fields(&self) -> DurationFields {
        let d = &self.0;
        DurationFields {
            years: d.years(),
            months: d.months(),
            weeks: d.weeks(),
            days: d.days(),
            hours: d.hours(),
            minutes: d.minutes(),
            seconds: d.seconds(),
            milliseconds: d.milliseconds(),
            microseconds: d.microseconds(),
            nanoseconds: d.nanoseconds(),
        }
    }

    /// -1, 0 or 1 (DurationSign).
    pub fn sign(&self) -> i8 {
        self.0.sign() as i8
    }

    pub fn negated(&self) -> Duration {
        Duration(self.0.negated())
    }

    pub fn abs(&self) -> Duration {
        Duration(self.0.abs())
    }

    /// AddDurations: a `RangeError` when either has years, months or
    /// weeks, or the sum is out of range.
    pub fn add(&self, other: &Duration) -> Result<Duration> {
        guard(|| Ok(created(self.0.add(&other.0)?)))
    }

    pub fn subtract(&self, other: &Duration) -> Result<Duration> {
        guard(|| Ok(created(self.0.subtract(&other.0)?)))
    }

    /// `Temporal.Duration.compare` after its arguments are read.
    pub fn compare(&self, other: &Duration, relative_to: Option<&RelativeTo>) -> Result<Ordering> {
        guard(|| {
            Ok(self.0.compare_with_provider(&other.0, relative_to.map(|r| r.0.clone()), &*COMPILED_TZ_PROVIDER)?)
        })
    }

    /// `Temporal.Duration.prototype.round` after its options are read.
    pub fn round(&self, options: RoundingOptions, relative_to: Option<&RelativeTo>) -> Result<Duration> {
        guard(|| {
            let options = options.to_rs()?;
            Ok(created(self.0.round_with_provider(
                options,
                relative_to.map(|r| r.0.clone()),
                &*COMPILED_TZ_PROVIDER,
            )?))
        })
    }

    /// `Temporal.Duration.prototype.total` after its options are read.
    pub fn total(&self, unit: Unit, relative_to: Option<&RelativeTo>) -> Result<f64> {
        guard(|| {
            let total =
                self.0.total_with_provider(unit.to_rs(), relative_to.map(|r| r.0.clone()), &*COMPILED_TZ_PROVIDER)?;
            Ok(total.as_inner())
        })
    }

    /// `Temporal.Duration.prototype.toString` after its options are read.
    pub fn to_string_with(&self, options: ToStringOptions) -> Result<String> {
        guard(|| Ok(self.0.as_temporal_string(options.to_rs())?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::RoundingMode;
    use crate::ErrorKind;

    fn d(s: &str) -> Duration {
        Duration::parse(s).unwrap()
    }

    #[test]
    fn durations_round_trip_through_strings_and_fields() {
        let x = d("P1Y2M3W4DT5H6M7.008009010S");
        let f = x.fields();
        assert_eq!((f.years, f.weeks, f.milliseconds, f.nanoseconds), (1, 3, 8, 10));
        assert_eq!(Duration::new(f).unwrap(), x);
        assert_eq!(x.to_string_with(ToStringOptions::default()).unwrap(), "P1Y2M3W4DT5H6M7.00800901S");
        assert_eq!(x.negated().sign(), -1);
        assert_eq!(x.negated().abs(), x);
    }

    #[test]
    fn results_keep_only_the_precision_of_a_number() {
        let max = (1i128 << 53) - 1;
        let a = Duration::new(DurationFields { microseconds: max, ..Default::default() }).unwrap();
        let b = Duration::new(DurationFields { microseconds: max - 1, ..Default::default() }).unwrap();
        let sum = a.add(&b).unwrap();
        assert_eq!(sum.fields().microseconds, 18_014_398_509_481_980);
        assert_eq!(sum.to_string_with(ToStringOptions::default()).unwrap(), "PT18014398509.48198S");
    }

    #[test]
    fn invalid_durations_are_range_errors() {
        let mixed = DurationFields { years: 1, days: -1, ..Default::default() };
        assert_eq!(Duration::new(mixed).unwrap_err().kind, ErrorKind::Range);
        let huge = DurationFields { years: 1 << 32, ..Default::default() };
        assert_eq!(Duration::new(huge).unwrap_err().kind, ErrorKind::Range);
        assert_eq!(Duration::parse("P").unwrap_err().kind, ErrorKind::Range);
        assert_eq!(d("P1Y").add(&d("P1D")).unwrap_err().kind, ErrorKind::Range);
    }

    #[test]
    fn rounding_and_totals_use_relative_to_for_calendar_units() {
        let options = RoundingOptions {
            largest_unit: Some(Unit::Hour),
            smallest_unit: Some(Unit::Minute),
            rounding_mode: Some(RoundingMode::HalfExpand),
            increment: Some(15),
        };
        assert_eq!(d("PT1H53M").round(options, None).unwrap(), d("PT2H"));
        // Months have no fixed length: totalling them needs a start date.
        assert_eq!(d("P1M").total(Unit::Day, None).unwrap_err().kind, ErrorKind::Range);
        let jan = RelativeTo::parse("2024-01-01").unwrap();
        let feb = RelativeTo::parse("2024-02-01").unwrap();
        assert_eq!(d("P1M").total(Unit::Day, Some(&jan)).unwrap(), 31.0);
        assert_eq!(d("P1M").total(Unit::Day, Some(&feb)).unwrap(), 29.0);
        assert_eq!(d("P1M").compare(&d("P30D"), Some(&feb)).unwrap(), Ordering::Less);
        let zoned = RelativeTo::parse("2024-03-10T00:00[America/New_York]").unwrap();
        assert_eq!(d("P1D").total(Unit::Hour, Some(&zoned)).unwrap(), 23.0);
    }
}
