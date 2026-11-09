//! IANA time zones: validity and canonical names from ICU4X's zone data,
//! UTC offsets from jiff's bundled tz database (ICU4X 2.4 carries no
//! transition rules), and the zones of a region from IANA's zone1970.tab.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use icu_time::zone::iana::IanaParserExtendedBorrowed;

fn parser() -> IanaParserExtendedBorrowed<'static> {
    IanaParserExtendedBorrowed::new()
}

/// Names ECMA-402 maps to `"UTC"` on canonicalisation.
fn is_utc_alias(canonical: &str) -> bool {
    matches!(
        canonical,
        "Etc/UTC" | "Etc/GMT" | "GMT" | "UTC" | "Etc/UCT" | "Etc/Universal" | "Etc/Zulu" | "Etc/Greenwich"
            | "Etc/GMT0" | "Etc/GMT+0" | "Etc/GMT-0"
    )
}

/// CanonicalizeTimeZoneName for a name that IsValidTimeZoneName accepts
/// (any case). `None` when the name is unknown.
pub fn canonicalize(name: &str) -> Option<String> {
    if !name.is_ascii() || name.is_empty() {
        return None;
    }
    if name.eq_ignore_ascii_case("utc") {
        return Some("UTC".to_string());
    }
    let r = parser().parse(name);
    if r.time_zone.is_unknown() {
        return None;
    }
    if is_utc_alias(r.canonical) {
        return Some("UTC".to_string());
    }
    // jiff must know the zone too, or we could not compute offsets.
    if jiff::tz::TimeZone::get(r.canonical).is_err() {
        return None;
    }
    Some(r.canonical.to_string())
}

/// Is `name` an IANA zone or link name (case-insensitively)?
pub fn is_valid(name: &str) -> bool {
    canonicalize(name).is_some()
}

/// AvailableTimeZones: every canonical name, sorted by code unit.
pub fn available() -> &'static [String] {
    static Z: OnceLock<Vec<String>> = OnceLock::new();
    Z.get_or_init(|| {
        let mut set = BTreeSet::new();
        for z in parser().iter() {
            if z.time_zone.is_unknown() {
                continue;
            }
            if let Some(c) = canonicalize(z.canonical) {
                set.insert(c);
            }
        }
        set.insert("UTC".to_string());
        set.into_iter().collect()
    })
}

/// The UTC offset in milliseconds of `zone` (a canonical name) at the
/// instant `epoch_ms`.
pub fn offset_ms(zone: &str, epoch_ms: f64) -> Option<f64> {
    if zone == "UTC" {
        return Some(0.0);
    }
    if let Some(off) = parse_offset_zone(zone) {
        return Some(off as f64);
    }
    let tz = jiff::tz::TimeZone::get(zone).ok()?;
    let ms = epoch_ms.clamp(-8.64e15, 8.64e15) as i64;
    let ts = jiff::Timestamp::from_millisecond(ms).ok()?;
    Some(tz.to_offset(ts).seconds() as f64 * 1000.0)
}

/// An offset time zone (`+05:30`, `-0800`) as ECMA-402 accepts for
/// `timeZone`: returns its offset in ms when `s` has that form.
pub fn parse_offset_zone(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 3 {
        return None;
    }
    let sign = match b[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let rest = &s[1..];
    let (hh, mm) = match rest.len() {
        2 => (&rest[..2], "00"),
        4 => (&rest[..2], &rest[2..]),
        5 if &rest[2..3] == ":" => (&rest[..2], &rest[3..]),
        _ => return None,
    };
    if !hh.bytes().all(|c| c.is_ascii_digit()) || !mm.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let h: i64 = hh.parse().ok()?;
    let m: i64 = mm.parse().ok()?;
    if h > 23 || m > 59 {
        return None;
    }
    Some(sign * (h * 60 + m) * 60_000)
}

/// The canonical spelling of an offset zone: `±HH:MM`.
pub fn format_offset_zone(offset_ms: i64) -> String {
    let sign = if offset_ms < 0 { '-' } else { '+' };
    let total = offset_ms.abs() / 60_000;
    format!("{sign}{:02}:{:02}", total / 60, total % 60)
}

/// The zones whose primary country is `region` (ISO 3166-1 alpha-2), as
/// canonical names sorted by code unit; `None` for an unknown region.
pub fn zones_for_region(region: &str) -> Option<Vec<String>> {
    let upper = region.to_ascii_uppercase();
    let (_, zones) = crate::tables::ZONES_BY_REGION.iter().find(|(r, _)| *r == upper)?;
    let mut out: Vec<String> = zones.iter().filter_map(|z| canonicalize(z)).collect();
    out.sort();
    out.dedup();
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_names() {
        assert_eq!(canonicalize("america/new_york").as_deref(), Some("America/New_York"));
        assert_eq!(canonicalize("Etc/GMT").as_deref(), Some("UTC"));
        assert_eq!(canonicalize("utc").as_deref(), Some("UTC"));
        assert!(canonicalize("Mars/Olympus").is_none());
        assert!(available().iter().any(|z| z == "Europe/Paris"));
        assert!(available().windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn offsets_follow_transitions() {
        // 2024-07-01T12:00Z: New York is on EDT (-4h).
        assert_eq!(offset_ms("America/New_York", 1_719_835_200_000.0), Some(-4.0 * 3_600_000.0));
        // 2024-01-01T12:00Z: EST (-5h).
        assert_eq!(offset_ms("America/New_York", 1_704_110_400_000.0), Some(-5.0 * 3_600_000.0));
        assert_eq!(offset_ms("Asia/Kolkata", 0.0), Some(5.5 * 3_600_000.0));
        assert_eq!(parse_offset_zone("+05:30"), Some(19_800_000));
        assert_eq!(parse_offset_zone("-0800"), Some(-28_800_000));
        assert_eq!(format_offset_zone(-28_800_000), "-08:00");
    }

    #[test]
    fn region_zones() {
        let us = zones_for_region("US").unwrap();
        assert!(us.iter().any(|z| z == "America/New_York"));
        assert!(zones_for_region("XX").is_none());
    }
}
