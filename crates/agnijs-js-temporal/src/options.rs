//! The option values Temporal methods read from their options bags, as
//! this crate's own types. The engine reads and coerces the bag in
//! specification order; these types only name the values it found.

use temporal_rs::options as rs;

/// A Temporal unit: the values of `largestUnit`, `smallestUnit` and
/// `unit`, in increasing size after `Auto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Unit {
    Auto,
    Nanosecond,
    Microsecond,
    Millisecond,
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Year,
}

impl Unit {
    /// The unit a singular or plural unit name, or `"auto"`, names
    /// (GetTemporalUnitValuedOption's list of allowed strings).
    pub fn parse(s: &str) -> Option<Unit> {
        Some(match s {
            "auto" => Unit::Auto,
            "year" | "years" => Unit::Year,
            "month" | "months" => Unit::Month,
            "week" | "weeks" => Unit::Week,
            "day" | "days" => Unit::Day,
            "hour" | "hours" => Unit::Hour,
            "minute" | "minutes" => Unit::Minute,
            "second" | "seconds" => Unit::Second,
            "millisecond" | "milliseconds" => Unit::Millisecond,
            "microsecond" | "microseconds" => Unit::Microsecond,
            "nanosecond" | "nanoseconds" => Unit::Nanosecond,
            _ => return None,
        })
    }

    /// Year, month, week or day.
    pub fn is_date(self) -> bool {
        matches!(self, Unit::Year | Unit::Month | Unit::Week | Unit::Day)
    }

    /// Hour or a smaller unit.
    pub fn is_time(self) -> bool {
        matches!(
            self,
            Unit::Hour | Unit::Minute | Unit::Second | Unit::Millisecond | Unit::Microsecond | Unit::Nanosecond
        )
    }

    pub(crate) fn to_rs(self) -> rs::Unit {
        match self {
            Unit::Auto => rs::Unit::Auto,
            Unit::Nanosecond => rs::Unit::Nanosecond,
            Unit::Microsecond => rs::Unit::Microsecond,
            Unit::Millisecond => rs::Unit::Millisecond,
            Unit::Second => rs::Unit::Second,
            Unit::Minute => rs::Unit::Minute,
            Unit::Hour => rs::Unit::Hour,
            Unit::Day => rs::Unit::Day,
            Unit::Week => rs::Unit::Week,
            Unit::Month => rs::Unit::Month,
            Unit::Year => rs::Unit::Year,
        }
    }
}

/// A `roundingMode` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundingMode {
    Ceil,
    Floor,
    Expand,
    Trunc,
    HalfCeil,
    HalfFloor,
    HalfExpand,
    HalfTrunc,
    HalfEven,
}

impl RoundingMode {
    pub fn parse(s: &str) -> Option<RoundingMode> {
        Some(match s {
            "ceil" => RoundingMode::Ceil,
            "floor" => RoundingMode::Floor,
            "expand" => RoundingMode::Expand,
            "trunc" => RoundingMode::Trunc,
            "halfCeil" => RoundingMode::HalfCeil,
            "halfFloor" => RoundingMode::HalfFloor,
            "halfExpand" => RoundingMode::HalfExpand,
            "halfTrunc" => RoundingMode::HalfTrunc,
            "halfEven" => RoundingMode::HalfEven,
            _ => return None,
        })
    }

    pub(crate) fn to_rs(self) -> rs::RoundingMode {
        match self {
            RoundingMode::Ceil => rs::RoundingMode::Ceil,
            RoundingMode::Floor => rs::RoundingMode::Floor,
            RoundingMode::Expand => rs::RoundingMode::Expand,
            RoundingMode::Trunc => rs::RoundingMode::Trunc,
            RoundingMode::HalfCeil => rs::RoundingMode::HalfCeil,
            RoundingMode::HalfFloor => rs::RoundingMode::HalfFloor,
            RoundingMode::HalfExpand => rs::RoundingMode::HalfExpand,
            RoundingMode::HalfTrunc => rs::RoundingMode::HalfTrunc,
            RoundingMode::HalfEven => rs::RoundingMode::HalfEven,
        }
    }
}

/// The options of `round`, `since` and `until`. `None` is an option the
/// bag left undefined; the operation applies its own default. The
/// increment is already checked to be in 1..=10⁹ by the engine
/// (GetRoundingIncrementOption).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RoundingOptions {
    pub largest_unit: Option<Unit>,
    pub smallest_unit: Option<Unit>,
    pub rounding_mode: Option<RoundingMode>,
    pub increment: Option<u32>,
}

impl RoundingOptions {
    pub(crate) fn to_rs(self) -> crate::Result<rs::RoundingOptions> {
        let mut o = rs::RoundingOptions::default();
        o.largest_unit = self.largest_unit.map(Unit::to_rs);
        o.smallest_unit = self.smallest_unit.map(Unit::to_rs);
        o.rounding_mode = self.rounding_mode.map(RoundingMode::to_rs);
        o.increment = self.increment.map(rs::RoundingIncrement::try_new).transpose()?;
        Ok(o)
    }
}

/// `fractionalSecondDigits`: `"auto"` or 0 to 9 digits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Precision {
    #[default]
    Auto,
    Digits(u8),
}

/// The options of `toString`: the precision of the seconds, and the
/// smallest unit and rounding mode, as read from the bag.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToStringOptions {
    pub precision: Precision,
    pub smallest_unit: Option<Unit>,
    pub rounding_mode: Option<RoundingMode>,
}

impl ToStringOptions {
    pub(crate) fn to_rs(self) -> rs::ToStringRoundingOptions {
        rs::ToStringRoundingOptions {
            precision: match self.precision {
                Precision::Auto => temporal_rs::parsers::Precision::Auto,
                Precision::Digits(d) => temporal_rs::parsers::Precision::Digit(d),
            },
            smallest_unit: self.smallest_unit.map(Unit::to_rs),
            rounding_mode: self.rounding_mode.map(RoundingMode::to_rs),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_parse_singular_plural_and_auto_only() {
        assert_eq!(Unit::parse("years"), Some(Unit::Year));
        assert_eq!(Unit::parse("nanosecond"), Some(Unit::Nanosecond));
        assert_eq!(Unit::parse("auto"), Some(Unit::Auto));
        assert_eq!(Unit::parse("Years"), None);
        assert_eq!(Unit::parse("era"), None);
        assert!(Unit::Day.is_date() && !Unit::Day.is_time());
        assert!(Unit::Hour.is_time() && !Unit::Auto.is_time());
    }

    #[test]
    fn rounding_modes_parse_the_nine_names() {
        assert_eq!(RoundingMode::parse("halfEven"), Some(RoundingMode::HalfEven));
        assert_eq!(RoundingMode::parse("halfeven"), None);
    }
}
