//! The Temporal seam of the AgniJS engine (ADR 0027).
//!
//! `temporal_rs` does the calendar, time zone and duration arithmetic of
//! the Temporal proposal. This crate wraps it behind types of its own so
//! the engine never names a `temporal_rs` type, and replacing or
//! upgrading the library touches this crate alone. The engine
//! (`agnijs-js-vm`'s `builtins/temporal`) owns everything JavaScript can
//! observe about the order of operations: reading options and property
//! bags, coercions, and which error is thrown first.
//!
//! The time zone data is the IANA database compiled into the binary
//! through `jiff-tzdb`, the same data `agnijs-js-intl` uses for
//! `Intl.DateTimeFormat`, so both agree on every offset. Nothing here
//! reads the system clock or the system time zone: the engine passes the
//! host's values in.

use std::fmt;

use temporal_rs::provider::COMPILED_TZ_PROVIDER;

/// What kind of JavaScript error a failed operation throws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// A value outside the range the specification allows.
    Range,
    /// A value of the wrong type or shape.
    Type,
    /// A broken invariant inside the library. The engine throws a plain
    /// `Error` for it instead of panicking; it is never expected.
    Internal,
}

/// A failed Temporal operation: the error kind to throw and its message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}

impl Error {
    pub fn range(message: impl Into<String>) -> Error {
        Error { kind: ErrorKind::Range, message: message.into() }
    }

    pub fn type_error(message: impl Into<String>) -> Error {
        Error { kind: ErrorKind::Type, message: message.into() }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<temporal_rs::TemporalError> for Error {
    fn from(e: temporal_rs::TemporalError) -> Error {
        use temporal_rs::error::ErrorKind as K;
        let kind = match e.kind() {
            K::Type => ErrorKind::Type,
            // Temporal throws RangeError for unparsable strings, never
            // SyntaxError; Generic errors are range failures too.
            K::Range | K::Syntax | K::Generic => ErrorKind::Range,
            K::Assert => ErrorKind::Internal,
        };
        Error { kind, message: e.into_message().to_string() }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// The canonical case of a time zone identifier the database knows
/// (`"america/new_york"` gives `"America/New_York"`), or of a UTC offset
/// time zone (`"+05:30"`). An unknown name is a `RangeError`.
pub fn time_zone_identifier(id: &str) -> Result<String> {
    let tz = temporal_rs::TimeZone::try_from_identifier_str_with_provider(id, &*COMPILED_TZ_PROVIDER)?;
    Ok(tz.identifier_with_provider(&*COMPILED_TZ_PROVIDER)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_zone_identifiers_are_case_normalized_from_the_compiled_database() {
        assert_eq!(time_zone_identifier("america/new_york").unwrap(), "America/New_York");
        assert_eq!(time_zone_identifier("UTC").unwrap(), "UTC");
        assert_eq!(time_zone_identifier("+05:30").unwrap(), "+05:30");
        assert_eq!(time_zone_identifier("Mars/Olympus_Mons").unwrap_err().kind, ErrorKind::Range);
    }
}
