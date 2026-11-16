//! `Temporal.Instant` values and time zones.

use temporal_rs::options::DifferenceSettings;
use temporal_rs::provider::COMPILED_TZ_PROVIDER;

use crate::duration::{created, Duration};
use crate::options::{RoundingOptions, ToStringOptions};
use crate::{guard, Result};

/// A time zone: a named zone from the compiled IANA database or a UTC
/// offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeZone(pub(crate) temporal_rs::TimeZone);

impl TimeZone {
    /// ParseTemporalTimeZoneString followed by the availability check of
    /// ToTemporalTimeZoneIdentifier: an identifier (`"Europe/Paris"`,
    /// `"+01:00"`) or an ISO 8601 string whose annotation or offset names
    /// the zone. Anything else, or a name the database does not know, is a
    /// `RangeError`.
    pub fn parse(s: &str) -> Result<TimeZone> {
        guard(|| Ok(TimeZone(temporal_rs::TimeZone::try_from_str_with_provider(s, &*COMPILED_TZ_PROVIDER)?)))
    }

    /// The identifier as the specification prints it: the database's
    /// case for a name, `±HH:MM` for an offset.
    pub fn identifier(&self) -> Result<String> {
        guard(|| Ok(self.0.identifier_with_provider(&*COMPILED_TZ_PROVIDER)?))
    }
}

/// An exact time ([[InitializedTemporalInstant]]): nanoseconds since the
/// epoch, within ±10⁸ days of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Instant(pub(crate) temporal_rs::Instant);

pub(crate) fn settings(o: RoundingOptions) -> Result<DifferenceSettings> {
    let rs = o.to_rs()?;
    let mut s = DifferenceSettings::default();
    s.largest_unit = rs.largest_unit;
    s.smallest_unit = rs.smallest_unit;
    s.rounding_mode = rs.rounding_mode;
    s.increment = rs.increment;
    Ok(s)
}

impl Instant {
    /// CreateTemporalInstant: a `RangeError` outside the representable
    /// range (IsValidEpochNanoseconds).
    pub fn new(epoch_nanoseconds: i128) -> Result<Instant> {
        guard(|| Ok(Instant(temporal_rs::Instant::try_new(epoch_nanoseconds)?)))
    }

    /// An ISO 8601 date-time with a UTC offset or `Z`
    /// (ParseTemporalInstantString).
    pub fn parse(s: &str) -> Result<Instant> {
        guard(|| Ok(Instant(temporal_rs::Instant::from_utf8(s.as_bytes())?)))
    }

    pub fn epoch_nanoseconds(&self) -> i128 {
        self.0.as_i128()
    }

    /// The epoch milliseconds, rounded towards negative infinity.
    pub fn epoch_milliseconds(&self) -> i64 {
        self.0.epoch_milliseconds()
    }

    /// AddDurationToInstant: a `RangeError` when the duration has calendar
    /// units or the result is out of range.
    pub fn add(&self, d: &Duration) -> Result<Instant> {
        guard(|| Ok(Instant(self.0.add(&d.0)?)))
    }

    pub fn subtract(&self, d: &Duration) -> Result<Instant> {
        guard(|| Ok(Instant(self.0.subtract(&d.0)?)))
    }

    /// DifferenceTemporalInstant(until) after the settings are read.
    pub fn until(&self, other: &Instant, options: RoundingOptions) -> Result<Duration> {
        guard(|| Ok(created(self.0.until(&other.0, settings(options)?)?)))
    }

    /// DifferenceTemporalInstant(since) after the settings are read.
    pub fn since(&self, other: &Instant, options: RoundingOptions) -> Result<Duration> {
        guard(|| Ok(created(self.0.since(&other.0, settings(options)?)?)))
    }

    /// `Temporal.Instant.prototype.round` after its options are read.
    pub fn round(&self, options: RoundingOptions) -> Result<Instant> {
        guard(|| Ok(Instant(self.0.round(options.to_rs()?)?)))
    }

    /// TemporalInstantToString: UTC with `Z`, or the wall-clock time and
    /// offset in `time_zone`.
    pub fn to_string_with(&self, options: ToStringOptions, time_zone: Option<&TimeZone>) -> Result<String> {
        guard(|| {
            Ok(self.0.to_ixdtf_string_with_provider(time_zone.map(|t| t.0), options.to_rs(), &*COMPILED_TZ_PROVIDER)?)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::{Precision, Unit};
    use crate::ErrorKind;

    #[test]
    fn instants_parse_print_and_stay_in_range() {
        let i = Instant::parse("2020-01-01T00:00:00.5+01:00").unwrap();
        assert_eq!(i.to_string_with(ToStringOptions::default(), None).unwrap(), "2019-12-31T23:00:00.5Z");
        assert_eq!(Instant::new(-1).unwrap().epoch_milliseconds(), -1);
        let limit = 8_640_000_000_000_000_000_000i128;
        assert!(Instant::new(limit).is_ok());
        assert_eq!(Instant::new(limit + 1).unwrap_err().kind, ErrorKind::Range);
        assert_eq!(Instant::parse("2020-01-01").unwrap_err().kind, ErrorKind::Range);
    }

    #[test]
    fn differences_and_time_zones() {
        let a = Instant::parse("2020-01-01T00:00Z").unwrap();
        let b = Instant::parse("2020-01-02T01:30Z").unwrap();
        let hours = RoundingOptions { largest_unit: Some(Unit::Hour), ..Default::default() };
        let d = a.until(&b, hours).unwrap();
        assert_eq!(d.to_string_with(ToStringOptions::default()).unwrap(), "PT25H30M");
        let ny = TimeZone::parse("america/new_york").unwrap();
        assert_eq!(ny.identifier().unwrap(), "America/New_York");
        let millis = ToStringOptions { precision: Precision::Digits(3), ..Default::default() };
        assert_eq!(a.to_string_with(millis, Some(&ny)).unwrap(), "2019-12-31T19:00:00.000-05:00");
        assert_eq!(TimeZone::parse("Mars/Olympus_Mons").unwrap_err().kind, ErrorKind::Range);
    }
}
