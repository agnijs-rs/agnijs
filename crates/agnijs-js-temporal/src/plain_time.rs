//! `Temporal.PlainTime` values.

use std::cmp::Ordering;

use temporal_rs::partial::PartialTime;

use crate::duration::{created, Duration};
use crate::instant::settings;
use crate::options::{Overflow, RoundingOptions, ToStringOptions};
use crate::{guard, Error, Result};

/// The time fields of a property bag after ToIntegerWithTruncation;
/// `None` is a field the bag left undefined. The engine converts numbers
/// saturating, so every value outside a field's range stays outside it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TimeFields {
    pub hour: Option<i64>,
    pub minute: Option<i64>,
    pub second: Option<i64>,
    pub millisecond: Option<i64>,
    pub microsecond: Option<i64>,
    pub nanosecond: Option<i64>,
}

impl TimeFields {
    pub fn is_empty(&self) -> bool {
        *self == TimeFields::default()
    }

    /// The first half of RegulateTime: with `Constrain` a value is clamped
    /// to its field's range, with `Reject` a value outside it is a
    /// `RangeError`. The library finishes the job for the values that fit
    /// its field types.
    fn to_rs(self, overflow: Overflow) -> Result<PartialTime> {
        fn field(v: Option<i64>, max: i64, overflow: Overflow) -> Result<Option<i64>> {
            match v {
                None => Ok(None),
                Some(v) if (0..=max).contains(&v) => Ok(Some(v)),
                Some(v) if overflow == Overflow::Constrain => Ok(Some(v.clamp(0, max))),
                Some(_) => Err(Error::range("a time field is out of range")),
            }
        }
        let mut p = PartialTime::default();
        p.hour = field(self.hour, 23, overflow)?.map(|v| v as u8);
        p.minute = field(self.minute, 59, overflow)?.map(|v| v as u8);
        p.second = field(self.second, 59, overflow)?.map(|v| v as u8);
        p.millisecond = field(self.millisecond, 999, overflow)?.map(|v| v as u16);
        p.microsecond = field(self.microsecond, 999, overflow)?.map(|v| v as u16);
        p.nanosecond = field(self.nanosecond, 999, overflow)?.map(|v| v as u16);
        Ok(p)
    }
}

/// A wall-clock time ([[InitializedTemporalTime]]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlainTime(pub(crate) temporal_rs::PlainTime);

impl PlainTime {
    /// CreateTemporalTime from the constructor's fields: a `RangeError`
    /// unless they are a valid time (IsValidTime).
    pub fn new(f: [i64; 6]) -> Result<PlainTime> {
        let fields = TimeFields {
            hour: Some(f[0]),
            minute: Some(f[1]),
            second: Some(f[2]),
            millisecond: Some(f[3]),
            microsecond: Some(f[4]),
            nanosecond: Some(f[5]),
        };
        PlainTime::from_fields(&fields, Overflow::Reject)
    }

    /// ToTemporalTimeRecord's fields (missing ones are zero) regulated by
    /// `overflow` (RegulateTime). A `TypeError` when no field is set.
    pub fn from_fields(fields: &TimeFields, overflow: Overflow) -> Result<PlainTime> {
        guard(|| {
            if fields.is_empty() {
                return Err(Error::type_error("a time-like object needs at least one time property"));
            }
            let mut complete = *fields;
            for f in [
                &mut complete.hour,
                &mut complete.minute,
                &mut complete.second,
                &mut complete.millisecond,
                &mut complete.microsecond,
                &mut complete.nanosecond,
            ] {
                f.get_or_insert(0);
            }
            let partial = complete.to_rs(overflow)?;
            Ok(PlainTime(temporal_rs::PlainTime::from_partial(partial, Some(overflow.to_rs()))?))
        })
    }

    /// ParseTemporalTimeString: a time, or a date-time without a `Z`
    /// designator whose time part is taken.
    pub fn parse(s: &str) -> Result<PlainTime> {
        guard(|| Ok(PlainTime(temporal_rs::PlainTime::from_utf8(s.as_bytes())?)))
    }

    /// The fields of `self` with those `fields` sets replaced, regulated
    /// by `overflow`. A `TypeError` when no field is set.
    pub fn with(&self, fields: &TimeFields, overflow: Overflow) -> Result<PlainTime> {
        guard(|| {
            if fields.is_empty() {
                return Err(Error::type_error("a time-like object needs at least one time property"));
            }
            let current = TimeFields {
                hour: Some(i64::from(self.hour())),
                minute: Some(i64::from(self.minute())),
                second: Some(i64::from(self.second())),
                millisecond: Some(i64::from(self.millisecond())),
                microsecond: Some(i64::from(self.microsecond())),
                nanosecond: Some(i64::from(self.nanosecond())),
            };
            let merged = TimeFields {
                hour: fields.hour.or(current.hour),
                minute: fields.minute.or(current.minute),
                second: fields.second.or(current.second),
                millisecond: fields.millisecond.or(current.millisecond),
                microsecond: fields.microsecond.or(current.microsecond),
                nanosecond: fields.nanosecond.or(current.nanosecond),
            };
            PlainTime::from_fields(&merged, overflow)
        })
    }

    /// CompareTimeRecord.
    pub fn compare(&self, other: &PlainTime) -> Ordering {
        let key = |t: &PlainTime| (t.hour(), t.minute(), t.second(), t.millisecond(), t.microsecond(), t.nanosecond());
        key(self).cmp(&key(other))
    }

    /// AddDurationToTime: the time wraps around midnight.
    pub fn add(&self, d: &Duration) -> Result<PlainTime> {
        guard(|| Ok(PlainTime(self.0.add(&d.0)?)))
    }

    pub fn subtract(&self, d: &Duration) -> Result<PlainTime> {
        guard(|| Ok(PlainTime(self.0.subtract(&d.0)?)))
    }

    /// DifferenceTemporalPlainTime(until) after the settings are read.
    pub fn until(&self, other: &PlainTime, options: RoundingOptions) -> Result<Duration> {
        guard(|| Ok(created(self.0.until(&other.0, settings(options)?)?)))
    }

    pub fn since(&self, other: &PlainTime, options: RoundingOptions) -> Result<Duration> {
        guard(|| Ok(created(self.0.since(&other.0, settings(options)?)?)))
    }

    /// `Temporal.PlainTime.prototype.round` after its options are read.
    pub fn round(&self, options: RoundingOptions) -> Result<PlainTime> {
        guard(|| Ok(PlainTime(self.0.round(options.to_rs()?)?)))
    }

    /// TimeRecordToString after RoundTime.
    pub fn to_string_with(&self, options: ToStringOptions) -> Result<String> {
        guard(|| Ok(self.0.to_ixdtf_string(options.to_rs())?))
    }

    pub fn hour(&self) -> u8 {
        self.0.hour()
    }
    pub fn minute(&self) -> u8 {
        self.0.minute()
    }
    pub fn second(&self) -> u8 {
        self.0.second()
    }
    pub fn millisecond(&self) -> u16 {
        self.0.millisecond()
    }
    pub fn microsecond(&self) -> u16 {
        self.0.microsecond()
    }
    pub fn nanosecond(&self) -> u16 {
        self.0.nanosecond()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::{Precision, Unit};
    use crate::ErrorKind;

    fn time(s: &str) -> PlainTime {
        PlainTime::parse(s).unwrap()
    }

    #[test]
    fn times_are_created_regulated_and_printed() {
        let t = PlainTime::new([13, 5, 9, 1, 2, 3]).unwrap();
        assert_eq!(t.to_string_with(ToStringOptions::default()).unwrap(), "13:05:09.001002003");
        assert_eq!(PlainTime::new([24, 0, 0, 0, 0, 0]).unwrap_err().kind, ErrorKind::Range);
        assert_eq!(PlainTime::new([-1, 0, 0, 0, 0, 0]).unwrap_err().kind, ErrorKind::Range);
        let bag = TimeFields { hour: Some(-5), minute: Some(75), ..Default::default() };
        assert_eq!(PlainTime::from_fields(&bag, Overflow::Constrain).unwrap(), time("00:59"));
        assert_eq!(PlainTime::from_fields(&bag, Overflow::Reject).unwrap_err().kind, ErrorKind::Range);
        assert_eq!(
            PlainTime::from_fields(&TimeFields::default(), Overflow::Constrain).unwrap_err().kind,
            ErrorKind::Type
        );
        assert_eq!(PlainTime::parse("2020-01-01T12:00Z").unwrap_err().kind, ErrorKind::Range);
        assert_eq!(
            time("T12:30").with(&TimeFields { second: Some(61), ..Default::default() }, Overflow::Constrain).unwrap(),
            time("12:30:59")
        );
    }

    #[test]
    fn arithmetic_wraps_and_differences_round() {
        let t = time("23:30");
        assert_eq!(t.add(&Duration::parse("PT45M").unwrap()).unwrap(), time("00:15"));
        assert_eq!(t.subtract(&Duration::parse("P1DT1H").unwrap()).unwrap(), time("22:30"));
        let minutes = RoundingOptions { largest_unit: Some(Unit::Minute), ..Default::default() };
        let d = time("10:00").until(&t, minutes).unwrap();
        assert_eq!(d.to_string_with(ToStringOptions::default()).unwrap(), "PT810M");
        let hour = RoundingOptions { smallest_unit: Some(Unit::Hour), ..Default::default() };
        assert_eq!(t.round(hour).unwrap(), time("00:00"));
        assert_eq!(t.compare(&time("23:30:00.000000001")), Ordering::Less);
        let digits = ToStringOptions { precision: Precision::Digits(2), ..Default::default() };
        assert_eq!(time("01:02:03.456").to_string_with(digits).unwrap(), "01:02:03.45");
    }
}
