//! `Intl.DateTimeFormat` services on ICU4X's semantic-skeleton formatter.
//!
//! ECMA-402 describes formats as component bags (`year: "numeric"`,
//! `month: "long"`, …) matched against locale patterns; ICU4X describes
//! them as field sets with a length. [`DateTimeFormatter::new`] maps the
//! bag to the nearest field set, and the resolved options are read back
//! from the pattern ICU4X actually selected, which is what the
//! specification's format matcher reports too.

use std::fmt;

use icu_calendar::Date;
use icu_datetime::fieldsets::builder::{DateFields, FieldSetBuilder, ZoneStyle};
use icu_datetime::fieldsets::enums::CompositeFieldSet;
use icu_datetime::options::{Alignment, Length, SubsecondDigits, TimePrecision, YearStyle};
use icu_datetime::DateTimeFormatterPreferences;
use icu_locale_core::Locale;
use icu_time::zone::iana::IanaParserBorrowed;
use icu_time::zone::{TimeZone, UtcOffset};
use icu_time::{DateTime, ZonedDateTime};
use writeable::{PartsWrite, Writeable};

pub use crate::number::{Part, RangePart};

/// The ECMA-402 component bag after option resolution. Each entry holds
/// the requested width string (`"numeric"`, `"2-digit"`, `"long"`, …).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Components {
    pub weekday: Option<String>,
    pub era: Option<String>,
    pub year: Option<String>,
    pub month: Option<String>,
    pub day: Option<String>,
    pub day_period: Option<String>,
    pub hour: Option<String>,
    pub minute: Option<String>,
    pub second: Option<String>,
    pub fractional_second_digits: Option<u8>,
    pub time_zone_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DateTimeFormatOptions {
    /// The resolved locale, with `-u-ca`/`-u-nu`/`-u-hc` applied.
    pub locale: String,
    /// The time zone: `UTC`, a canonical IANA name, or `±HH:MM`.
    pub time_zone: String,
    pub components: Components,
    pub date_style: Option<String>,
    pub time_style: Option<String>,
}

/// What the selected pattern contains, for `resolvedOptions()`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resolved {
    /// `h11`, `h12`, `h23` or `h24` when the pattern has an hour.
    pub hour_cycle: Option<&'static str>,
    pub components: Components,
}

pub struct DateTimeFormatter {
    fmt: icu_datetime::DateTimeFormatter<CompositeFieldSet>,
    range: Option<icu_datetime::range::DateRangeFormatter<CompositeFieldSet>>,
    zone: ZoneSpec,
    resolved: Resolved,
    /// ICU4X has no hour-less time patterns: minute/second bags are
    /// formatted with an `h23` hour that is removed from the output again.
    strip_hour: bool,
    /// A second-only bag also drops the minute.
    strip_minute: bool,
    /// `hc-h24` is formatted as `h23` and midnight rewritten to 24.
    h24: bool,
    /// The numbering system's decimal separator, for fractional seconds.
    decimal_sep: String,
    /// ICU4X ties the weekday width to the month width; when the bag asks
    /// for a different weekday width this formats the weekday alone.
    weekday_fmt: Option<icu_datetime::DateTimeFormatter<CompositeFieldSet>>,
    /// `dateStyle: "short"` in a locale whose short pattern has a two-digit
    /// year: ICU4X widens distant years (and adds an era), ECMA-402
    /// implementations keep the pattern's two digits.
    two_digit_year: bool,
}

#[derive(Clone, Debug)]
enum ZoneSpec {
    Utc,
    Iana(String, TimeZone),
    Offset(i64),
}

impl DateTimeFormatter {
    pub fn new(opts: &DateTimeFormatOptions) -> Result<DateTimeFormatter, String> {
        let c = &opts.components;
        let styled = opts.date_style.is_some() || opts.time_style.is_some();
        let strip_hour = !styled && c.hour.is_none() && (c.minute.is_some() || c.second.is_some() || c.fractional_second_digits.is_some());
        let strip_minute = strip_hour && c.minute.is_none();
        let (_, keywords) = crate::locale::split_unicode_extension(&opts.locale);
        let hc = keywords.iter().find(|(k, _)| k == "hc").map(|(_, v)| v.clone());
        let h24 = hc.as_deref() == Some("h24");
        let mut tag = opts.locale.clone();
        if strip_hour || h24 {
            tag = crate::locale::with_unicode_keywords(&tag, &[("hc", Some("h23"))]);
        }
        let locale: Locale = tag.parse().map_err(|_| format!("bad locale {}", opts.locale))?;
        let prefs = DateTimeFormatterPreferences::from(&locale);
        let fieldset = field_set(opts, None)?;
        let fmt = icu_datetime::DateTimeFormatter::try_new(prefs.clone(), fieldset.clone()).map_err(|e| e.to_string())?;
        // The weekday keeps its own width: the same field set at the
        // weekday's length, from which only the weekday part is taken (so
        // the format form "Sa.," is used, not the stand-alone "Sa").
        let weekday_fmt = match (&c.weekday, &c.month, styled) {
            (Some(w), Some(m), false) if length_of(w) != length_of(m) || w == "narrow" => {
                let len = if w == "narrow" { Length::Short } else { length_of(w) };
                field_set(opts, Some(len)).ok().and_then(|fs| icu_datetime::DateTimeFormatter::try_new(prefs.clone(), fs).ok())
            }
            _ => None,
        };
        let range = icu_datetime::range::DateRangeFormatter::try_new(prefs, fieldset).ok();
        let zone = zone_spec(&opts.time_zone)?;
        let nu = keywords.iter().find(|(k, _)| k == "nu").map(|(_, v)| v.clone()).unwrap_or_default();
        let decimal_sep = crate::number::decimal_separator(&opts.locale, &nu);
        let mut f = DateTimeFormatter {
            fmt,
            range,
            zone,
            resolved: Resolved::default(),
            strip_hour,
            strip_minute,
            h24,
            decimal_sep,
            weekday_fmt,
            two_digit_year: false,
        };
        let sample = f.formatted(0.0);
        let pattern = sample.pattern().write_to_string().into_owned();
        f.resolved = resolve(&pattern, opts);
        if opts.date_style.as_deref() == Some("short") {
            let year_1970 = f.format_to_parts(0.0).into_iter().find(|p| p.kind == "year").map(|p| p.value);
            f.two_digit_year = matches!(year_1970.as_deref(), Some(y) if y.chars().count() == 2);
        }
        if strip_hour {
            f.resolved.components.hour = None;
            f.resolved.hour_cycle = None;
            if strip_minute {
                f.resolved.components.minute = None;
            }
        }
        if h24 && f.resolved.hour_cycle.is_some() {
            f.resolved.hour_cycle = Some("h24");
        }
        Ok(f)
    }

    pub fn resolved(&self) -> &Resolved {
        &self.resolved
    }

    fn zoned(&self, epoch_ms: f64) -> ZonedDateTime<icu_calendar::Iso, icu_time::TimeZoneInfo<icu_time::zone::models::AtTime>> {
        let ms = epoch_ms.clamp(-8.64e15, 8.64e15) as i64;
        let (id, offset_ms) = match &self.zone {
            ZoneSpec::Utc => (TimeZone(icu_locale_core::subtags::subtag!("utc")), 0),
            ZoneSpec::Iana(name, id) => (*id, crate::timezone::offset_ms(name, epoch_ms).unwrap_or(0.0) as i64),
            ZoneSpec::Offset(o) => (TimeZone::UNKNOWN, *o),
        };
        let offset = UtcOffset::try_from_seconds((offset_ms / 1000) as i32).unwrap_or(UtcOffset::zero());
        let local = ZonedDateTime::from_epoch_milliseconds_and_utc_offset(ms, offset);
        let dt = DateTime { date: local.date, time: local.time };
        let zone = id.with_offset(Some(offset)).at_date_time(dt);
        ZonedDateTime { date: local.date, time: local.time, zone }
    }

    fn formatted(&self, epoch_ms: f64) -> icu_datetime::FormattedDateTime<'_> {
        let zdt = self.zoned(epoch_ms);
        self.fmt.format(&zdt)
    }

    pub fn format_to_parts(&self, epoch_ms: f64) -> Vec<Part> {
        let f = self.formatted(epoch_ms);
        let mut sink = Sink::default();
        let _ = f.write_to_parts(&mut sink);
        let mut parts = sink.finish(self.resolved.components.fractional_second_digits.is_some(), &self.decimal_sep);
        self.fix_parts(&mut parts, epoch_ms, epoch_ms);
        parts.into_iter().map(|p| Part { kind: p.kind, value: p.value }).collect()
    }

    /// FormatDateTime: the parts joined.
    pub fn format(&self, epoch_ms: f64) -> String {
        self.format_to_parts(epoch_ms).iter().map(|p| p.value.as_str()).collect()
    }

    fn weekday_text(&self, epoch_ms: f64) -> Option<String> {
        let w = self.weekday_fmt.as_ref()?;
        let zdt = self.zoned(epoch_ms);
        let mut sink = Sink::default();
        w.format(&zdt).write_to_parts(&mut sink).ok()?;
        sink.finish(false, ".").into_iter().find(|p| p.kind == "weekday").map(|p| p.value)
    }

    /// Post-process ICU4X's parts: remove the synthesized hour (and
    /// minute) with the literal that follows each, rewrite midnight for
    /// `h24`, swap in the requested weekday width, and keep two-digit
    /// years two digits. `a`/`b` are the instants of the start and end
    /// sides (equal for a single date).
    fn fix_parts(&self, parts: &mut Vec<RangePart>, a: f64, b: f64) {
        if self.strip_hour {
            let mut out = Vec::with_capacity(parts.len());
            let mut skip_literal = false;
            for p in parts.drain(..) {
                if skip_literal && p.kind == "literal" {
                    skip_literal = false;
                    continue;
                }
                skip_literal = false;
                if p.kind == "hour" || (self.strip_minute && p.kind == "minute") || p.kind == "dayPeriod" {
                    skip_literal = true;
                    continue;
                }
                out.push(p);
            }
            *parts = out;
        }
        if self.h24 {
            for p in parts.iter_mut() {
                if p.kind == "hour" && p.value.chars().all(|c| c == '0' || !c.is_ascii_digit()) {
                    p.value = "24".to_string();
                }
            }
        }
        if self.weekday_fmt.is_some() {
            for p in parts.iter_mut() {
                if p.kind == "weekday" {
                    let t = if p.source == "endRange" { b } else { a };
                    if let Some(w) = self.weekday_text(t) {
                        p.value = w;
                    }
                }
            }
        }
        if self.two_digit_year {
            let mut out = Vec::with_capacity(parts.len());
            let mut drop_literal = false;
            for mut p in parts.drain(..) {
                if drop_literal && p.kind == "literal" {
                    drop_literal = false;
                    continue;
                }
                drop_literal = false;
                if p.kind == "era" {
                    // The era came with the widened year; drop it and the
                    // literal that separated it.
                    if let Some(last) = out.last() {
                        if (last as &RangePart).kind == "literal" {
                            out.pop();
                        }
                    }
                    drop_literal = true;
                    continue;
                }
                if p.kind == "year" && p.value.chars().all(|c| c.is_ascii_digit()) && p.value.len() > 2 {
                    p.value = p.value[p.value.len() - 2..].to_string();
                }
                out.push(p);
            }
            *parts = out;
        }
    }

    /// PartitionDateTimeRangePattern: ICU4X's interval format when it has
    /// one for this field set, else the two values joined by the
    /// locale-independent fallback.
    pub fn format_range_to_parts(&self, a: f64, b: f64) -> Vec<RangePart> {
        let pa = self.format_to_parts(a);
        let pb = self.format_to_parts(b);
        // Equal formatted dates are "practically equal": the single format.
        if pa == pb {
            return pa.into_iter().map(|p| RangePart { kind: p.kind, value: p.value, source: "shared" }).collect();
        }
        // Synthesized bags and two-digit years cannot be read back out of
        // ICU4X's interval text; those join the two single formats.
        if let (Some(r), false) = (&self.range, self.strip_hour || self.two_digit_year) {
            let za = self.zoned(a);
            let mut zb = self.zoned(b);
            // ICU4X treats zone infos taken at different instants as
            // different zones; a range within one offset shares the zone.
            if za.zone.offset() == zb.zone.offset() {
                zb.zone = za.zone;
            }
            let f = r.format(&za, &zb);
            let mut sink = Sink::default();
            if f.write_to_parts(&mut sink).is_ok() {
                let parts = sink.finish(self.resolved.components.fractional_second_digits.is_some(), &self.decimal_sep);
                if let Some(mut parts) = annotate_range(parts, &pa, &pb) {
                    self.fix_parts(&mut parts, a, b);
                    return parts;
                }
            }
        }
        let mut out: Vec<RangePart> =
            pa.into_iter().map(|p| RangePart { kind: p.kind, value: p.value, source: "startRange" }).collect();
        out.push(RangePart { kind: "literal", value: "\u{2009}–\u{2009}".to_string(), source: "shared" });
        out.extend(pb.into_iter().map(|p| RangePart { kind: p.kind, value: p.value, source: "endRange" }));
        out
    }

    pub fn format_range(&self, a: f64, b: f64) -> String {
        self.format_range_to_parts(a, b).iter().map(|p| p.value.as_str()).collect()
    }
}

/// ICU4X's range formatter marks the two sides but drops the field parts
/// inside them (and inside a collapsed range the shared fields too).
/// Recover them by aligning each side's text with the single-date parts:
/// a start run is a prefix of the start date's parts plus the range
/// separator, an end run is a suffix of the end date's parts. In a
/// collapsed range (`Jan 3 – 5, 2019`) the fields the two dates agree on
/// at the start and end are shared, as the CLDR interval pattern has them.
/// `None` when the interval text does not read as the single formats
/// (ICU4X picked a different skeleton for the interval, as it does for the
/// Chinese calendar): the caller joins the single formats instead.
fn annotate_range(runs: Vec<RangePart>, pa: &[Part], pb: &[Part]) -> Option<Vec<RangePart>> {
    let plain = |s: &str| !s.chars().any(|c| c.is_alphanumeric());
    enum Run {
        Keep(RangePart),
        Start(Aligned, String),
        End(Aligned, String),
        Shared(Vec<Part>),
    }
    let mut aligned = Vec::with_capacity(runs.len());
    for run in runs {
        if run.kind != "literal" {
            aligned.push(Run::Keep(run));
            continue;
        }
        match run.source {
            "startRange" => {
                let al = align_prefix(&run.value, pa)?;
                let rest = run.value[al.consumed..].to_string();
                if !plain(&rest) {
                    return None;
                }
                aligned.push(Run::Start(al, rest));
            }
            "endRange" => {
                let al = align_suffix(&run.value, pb)?;
                let rest = run.value[..run.value.len() - al.consumed].to_string();
                if !plain(&rest) {
                    return None;
                }
                aligned.push(Run::End(al, rest));
            }
            _ => match align_prefix(&run.value, pa)
                .filter(|al| al.consumed == run.value.len())
                .or_else(|| align_prefix(&run.value, pb).filter(|al| al.consumed == run.value.len()))
            {
                Some(al) => aligned.push(Run::Shared(al.parts)),
                None if plain(&run.value) => aligned.push(Run::Keep(run)),
                None => return None,
            },
        }
    }
    // A collapsed interval shows part of at least one side; the fields the
    // two dates agree on at its start and end then come from the pattern's
    // shared portion.
    let collapsed = aligned.iter().any(|r| match r {
        Run::Start(al, _) => !(al.start == 0 && al.end == pa.len()),
        Run::End(al, _) => !(al.start == 0 && al.end == pb.len()),
        _ => false,
    });
    let mut out = Vec::new();
    for r in aligned {
        match r {
            Run::Keep(run) => out.push(run),
            Run::Shared(parts) => out.extend(parts.into_iter().map(|p| RangePart { kind: p.kind, value: p.value, source: "shared" })),
            Run::Start(al, rest) => {
                let mut shared_prefix = collapsed && al.start == 0;
                for (i, p) in al.parts.into_iter().enumerate() {
                    if shared_prefix && !(i < pb.len() && pb[i].kind == p.kind && pb[i].value == p.value) {
                        shared_prefix = false;
                    }
                    out.push(RangePart { kind: p.kind, value: p.value, source: if shared_prefix { "shared" } else { "startRange" } });
                }
                if !rest.is_empty() {
                    out.push(RangePart { kind: "literal", value: rest, source: "shared" });
                }
            }
            Run::End(al, rest) => {
                if !rest.is_empty() {
                    out.push(RangePart { kind: "literal", value: rest, source: "shared" });
                }
                let n = al.parts.len();
                let mut sources = vec!["endRange"; n];
                if collapsed && al.end == pb.len() {
                    for t in 0..n {
                        let p = &al.parts[n - 1 - t];
                        let same = pa.len() > t && pa[pa.len() - 1 - t].kind == p.kind && pa[pa.len() - 1 - t].value == p.value;
                        if !same {
                            break;
                        }
                        sources[n - 1 - t] = "shared";
                    }
                }
                for (p, src) in al.parts.into_iter().zip(sources) {
                    out.push(RangePart { kind: p.kind, value: p.value, source: src });
                }
            }
        }
    }
    Some(out)
}

struct Aligned {
    parts: Vec<Part>,
    /// Bytes of the text covered (from its start for a prefix alignment,
    /// from its end for a suffix alignment).
    consumed: usize,
    /// The covered index range of the single-date parts.
    start: usize,
    end: usize,
}

/// The longest prefix of `text` that reads as consecutive `parts`
/// (allowing a partial literal at either end), covering at least one
/// non-literal part.
fn align_prefix(text: &str, parts: &[Part]) -> Option<Aligned> {
    let mut best: Option<Aligned> = None;
    for start in 0..parts.len() {
        let mut rest = text;
        let mut out: Vec<Part> = Vec::new();
        let mut k = start;
        let mut fields = 0;
        while !rest.is_empty() && k < parts.len() {
            let v = parts[k].value.as_str();
            if rest.starts_with(v) {
                out.push(parts[k].clone());
                if parts[k].kind != "literal" {
                    fields += 1;
                }
                rest = &rest[v.len()..];
                k += 1;
            } else if parts[k].kind == "literal" && v.starts_with(rest) {
                out.push(Part { kind: "literal", value: rest.to_string() });
                rest = "";
                k += 1;
            } else if parts[k].kind == "literal" && k == start {
                let suffix = (1..v.len()).rev().find(|&n| v.is_char_boundary(v.len() - n) && rest.starts_with(&v[v.len() - n..]));
                let Some(n) = suffix else { break };
                out.push(Part { kind: "literal", value: v[v.len() - n..].to_string() });
                rest = &rest[n..];
                k += 1;
            } else {
                break;
            }
        }
        let consumed = text.len() - rest.len();
        if fields > 0 && best.as_ref().map_or(true, |b| consumed > b.consumed) {
            best = Some(Aligned { parts: out, consumed, start, end: k });
        }
    }
    best
}

/// The suffix counterpart of [`align_prefix`], by reversing text and parts.
fn align_suffix(text: &str, parts: &[Part]) -> Option<Aligned> {
    let rev = |s: &str| s.chars().rev().collect::<String>();
    let rtext = rev(text);
    let rparts: Vec<Part> = parts.iter().rev().map(|p| Part { kind: p.kind, value: rev(&p.value) }).collect();
    let al = align_prefix(&rtext, &rparts)?;
    let n = parts.len();
    Some(Aligned {
        parts: al.parts.into_iter().rev().map(|p| Part { kind: p.kind, value: rev(&p.value) }).collect(),
        consumed: al.consumed,
        start: n - al.end,
        end: n - al.start,
    })
}

fn zone_spec(tz: &str) -> Result<ZoneSpec, String> {
    if tz == "UTC" {
        return Ok(ZoneSpec::Utc);
    }
    if let Some(off) = crate::timezone::parse_offset_zone(tz) {
        return Ok(ZoneSpec::Offset(off));
    }
    let id = IanaParserBorrowed::new().parse(tz);
    if id.is_unknown() {
        return Err(format!("unknown time zone {tz}"));
    }
    Ok(ZoneSpec::Iana(tz.to_string(), id))
}

fn length_of(width: &str) -> Length {
    match width {
        "long" => Length::Long,
        "short" | "narrow" => Length::Medium,
        _ => Length::Short,
    }
}

/// Map the component bag (or styles) onto an ICU4X field set.
fn field_set(opts: &DateTimeFormatOptions, length_override: Option<Length>) -> Result<CompositeFieldSet, String> {
    let c = &opts.components;
    let mut b = FieldSetBuilder::new();
    if opts.date_style.is_some() || opts.time_style.is_some() {
        if let Some(ds) = &opts.date_style {
            b.date_fields = Some(if ds == "full" { DateFields::YMDE } else { DateFields::YMD });
            b.length = Some(match ds.as_str() {
                "full" | "long" => Length::Long,
                "medium" => Length::Medium,
                _ => Length::Short,
            });
        }
        if let Some(ts) = &opts.time_style {
            b.time_precision = Some(if ts == "short" { TimePrecision::Minute } else { TimePrecision::Second });
            match ts.as_str() {
                "full" => b.zone_style = Some(ZoneStyle::SpecificLong),
                "long" => b.zone_style = Some(ZoneStyle::SpecificShort),
                _ => {}
            }
            if b.length.is_none() {
                b.length = Some(match ts.as_str() {
                    "full" | "long" => Length::Long,
                    "medium" => Length::Medium,
                    _ => Length::Short,
                });
            }
        }
        return b.build_composite().map_err(|e| format!("{e:?}"));
    }
    let (w, g, y, m, d) = (c.weekday.is_some(), c.era.is_some(), c.year.is_some(), c.month.is_some(), c.day.is_some());
    let date_fields = match (y || g, m, d, w) {
        (true, true, true, false) => Some(DateFields::YMD),
        (true, true, true, true) => Some(DateFields::YMDE),
        (false, true, true, false) => Some(DateFields::MD),
        (false, true, true, true) => Some(DateFields::MDE),
        (true, true, false, false) => Some(DateFields::YM),
        (true, true, false, true) => Some(DateFields::YMDE),
        (true, false, true, _) => Some(if w { DateFields::YMDE } else { DateFields::YMD }),
        (true, false, false, false) => Some(DateFields::Y),
        (true, false, false, true) => Some(DateFields::YMDE),
        (false, true, false, false) => Some(DateFields::M),
        (false, true, false, true) => Some(DateFields::MDE),
        (false, false, true, false) => Some(DateFields::D),
        (false, false, true, true) => Some(DateFields::DE),
        (false, false, false, true) => Some(DateFields::E),
        (false, false, false, false) => None,
    };
    b.date_fields = date_fields;
    let mut length = None;
    if let Some(mw) = &c.month {
        length = Some(length_of(mw));
    } else if let Some(ww) = &c.weekday {
        length = Some(length_of(ww));
    } else if date_fields.is_some() {
        length = Some(Length::Short);
    }
    if c.hour.is_some() || c.minute.is_some() || c.second.is_some() || c.fractional_second_digits.is_some() {
        b.time_precision = Some(if let Some(n) = c.fractional_second_digits {
            TimePrecision::Subsecond(SubsecondDigits::try_from_int(n.clamp(1, 9)).unwrap_or(SubsecondDigits::S3))
        } else if c.second.is_some() {
            TimePrecision::Second
        } else if c.minute.is_some() {
            TimePrecision::Minute
        } else {
            TimePrecision::Hour
        });
        if length.is_none() {
            length = Some(Length::Short);
        }
    }
    if let Some(z) = &c.time_zone_name {
        b.zone_style = Some(match z.as_str() {
            "long" => ZoneStyle::SpecificLong,
            "shortOffset" => ZoneStyle::LocalizedOffsetShort,
            "longOffset" => ZoneStyle::LocalizedOffsetLong,
            "shortGeneric" => ZoneStyle::GenericShort,
            "longGeneric" => ZoneStyle::GenericLong,
            _ => ZoneStyle::SpecificShort,
        });
        if length.is_none() {
            length = Some(Length::Short);
        }
    }
    b.length = length_override.or(length);
    let two_digit = |o: &Option<String>| o.as_deref() == Some("2-digit");
    if two_digit(&c.day) || two_digit(&c.month) || two_digit(&c.hour) {
        b.alignment = Some(Alignment::Column);
    }
    if y || g {
        b.year_style = Some(if g {
            YearStyle::WithEra
        } else if two_digit(&c.year) {
            YearStyle::Auto
        } else {
            YearStyle::Full
        });
    }
    b.build_composite().map_err(|e| format!("{e:?}"))
}

/// Read the selected pattern back into ECMA-402 component widths.
fn resolve(pattern: &str, opts: &DateTimeFormatOptions) -> Resolved {
    let mut r = Resolved::default();
    let mut in_quote = false;
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '\'' {
            in_quote = !in_quote;
            i += 1;
            continue;
        }
        if in_quote || !ch.is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let mut n = 1;
        while i + n < chars.len() && chars[i + n] == ch {
            n += 1;
        }
        i += n;
        let numeric = || if n >= 2 { "2-digit" } else { "numeric" }.to_string();
        let name = |short: &str| match n {
            4 => "long".to_string(),
            5 => "narrow".to_string(),
            _ => short.to_string(),
        };
        match ch {
            'G' => r.components.era = Some(name("short")),
            'y' | 'Y' | 'u' | 'U' | 'r' => r.components.year = Some(numeric()),
            'M' | 'L' => r.components.month = Some(if n >= 3 { name("short") } else { numeric() }),
            'd' => r.components.day = Some(numeric()),
            'E' | 'c' | 'e' => r.components.weekday = Some(name("short")),
            'B' | 'b' => r.components.day_period = Some(name("short")),
            'h' | 'H' | 'K' | 'k' => {
                r.components.hour = Some(numeric());
                r.hour_cycle = Some(match ch {
                    'h' => "h12",
                    'K' => "h11",
                    'k' => "h24",
                    _ => "h23",
                });
            }
            'm' => r.components.minute = Some(numeric()),
            's' => r.components.second = Some(numeric()),
            'S' => r.components.fractional_second_digits = Some(n.min(9) as u8),
            'z' => r.components.time_zone_name = Some(if n >= 4 { "long" } else { "short" }.to_string()),
            'O' => r.components.time_zone_name = Some(if n >= 4 { "longOffset" } else { "shortOffset" }.to_string()),
            'v' => r.components.time_zone_name = Some(if n >= 4 { "longGeneric" } else { "shortGeneric" }.to_string()),
            'V' | 'Z' | 'X' | 'x' => r.components.time_zone_name = Some("short".to_string()),
            _ => {}
        }
    }
    // ECMA-402 reports minute and second as 2-digit whenever present in
    // a pattern with padding; ICU4X always pads them.
    if r.components.minute.is_some() {
        r.components.minute = Some("2-digit".to_string());
    }
    if r.components.second.is_some() {
        r.components.second = Some("2-digit".to_string());
    }
    if opts.components.fractional_second_digits.is_some() && r.components.fractional_second_digits.is_none() {
        r.components.fractional_second_digits = opts.components.fractional_second_digits;
    }
    r
}

/// The hour cycle a locale uses by default (`h11`, `h12`, `h23` or `h24`).
pub fn default_hour_cycle(tag: &str) -> String {
    let opts = DateTimeFormatOptions {
        locale: crate::locale::base_name(tag),
        time_zone: "UTC".to_string(),
        components: Components { hour: Some("numeric".to_string()), ..Default::default() },
        date_style: None,
        time_style: None,
    };
    match DateTimeFormatter::new(&opts) {
        Ok(f) => f.resolved().hour_cycle.unwrap_or("h23").to_string(),
        Err(_) => "h23".to_string(),
    }
}

/// A `PartsWrite` sink that records each text run with its field part and
/// its range side.
#[derive(Default)]
struct Sink {
    runs: Vec<(Option<writeable::Part>, Option<&'static str>, String)>,
    stack: Vec<writeable::Part>,
}

impl fmt::Write for Sink {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let field = self.stack.iter().find(|p| p.category == "datetime").copied();
        let source = self.stack.iter().find_map(|p| range_source(*p));
        if let Some(last) = self.runs.last_mut() {
            if last.0 == field && last.1 == source {
                last.2.push_str(s);
                return Ok(());
            }
        }
        self.runs.push((field, source, s.to_string()));
        Ok(())
    }
}

impl PartsWrite for Sink {
    type SubPartsWrite = Sink;
    fn with_part(&mut self, part: writeable::Part, mut f: impl FnMut(&mut Sink) -> fmt::Result) -> fmt::Result {
        self.stack.push(part);
        let r = f(self);
        self.stack.pop();
        r
    }
}

fn range_source(p: writeable::Part) -> Option<&'static str> {
    if p == icu_datetime::parts::RANGE_START {
        Some("startRange")
    } else if p == icu_datetime::parts::RANGE_END {
        Some("endRange")
    } else {
        None
    }
}

fn part_kind(p: Option<writeable::Part>) -> &'static str {
    use icu_datetime::parts::*;
    match p {
        Some(p) if p == ERA => "era",
        Some(p) if p == YEAR => "year",
        Some(p) if p == RELATED_YEAR => "relatedYear",
        Some(p) if p == YEAR_NAME => "yearName",
        Some(p) if p == MONTH => "month",
        Some(p) if p == DAY => "day",
        Some(p) if p == WEEKDAY => "weekday",
        Some(p) if p == DAY_PERIOD => "dayPeriod",
        Some(p) if p == HOUR => "hour",
        Some(p) if p == MINUTE => "minute",
        Some(p) if p == SECOND => "second",
        Some(p) if p == TIME_ZONE_NAME => "timeZoneName",
        _ => "literal",
    }
}

impl Sink {
    fn finish(self, split_fraction: bool, decimal_sep: &str) -> Vec<RangePart> {
        let mut out = Vec::new();
        for (part, source, text) in self.runs {
            let kind = part_kind(part);
            let source = source.unwrap_or("shared");
            if kind == "second" && split_fraction {
                if let Some(i) = text.find(|c: char| !c.is_alphanumeric()) {
                    let (sec, rest) = text.split_at(i);
                    let sep_len = rest.chars().next().map(|c| c.len_utf8()).unwrap_or(0);
                    let (sep, frac) = rest.split_at(sep_len);
                    out.push(RangePart { kind: "second", value: sec.to_string(), source });
                    let _ = sep;
                    out.push(RangePart { kind: "literal", value: decimal_sep.to_string(), source });
                    out.push(RangePart { kind: "fractionalSecond", value: frac.to_string(), source });
                    continue;
                }
            }
            out.push(RangePart { kind, value: text, source });
        }
        out
    }
}

#[allow(dead_code)]
fn _date_type_check(d: Date<icu_calendar::Iso>) -> Date<icu_calendar::Iso> {
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(locale: &str, tz: &str, c: Components) -> DateTimeFormatOptions {
        DateTimeFormatOptions { locale: locale.into(), time_zone: tz.into(), components: c, date_style: None, time_style: None }
    }

    fn ymd_hms() -> Components {
        Components {
            year: Some("numeric".into()),
            month: Some("numeric".into()),
            day: Some("numeric".into()),
            hour: Some("numeric".into()),
            minute: Some("numeric".into()),
            second: Some("numeric".into()),
            ..Default::default()
        }
    }

    #[test]
    fn default_formats() {
        // 2024-07-01T12:34:56Z
        let t = 1_719_837_296_000.0;
        let f = DateTimeFormatter::new(&opts("en-US", "UTC", ymd_hms())).unwrap();
        assert_eq!(f.format(t), "7/1/2024, 12:34:56\u{202f}PM");
        assert_eq!(f.resolved().hour_cycle, Some("h12"));
        let f = DateTimeFormatter::new(&opts("en-US", "America/New_York", ymd_hms())).unwrap();
        assert_eq!(f.format(t), "7/1/2024, 8:34:56\u{202f}AM");
        let f = DateTimeFormatter::new(&opts("de-DE", "UTC", ymd_hms())).unwrap();
        assert_eq!(f.format(t), "01.07.2024, 12:34:56");
        assert_eq!(f.resolved().hour_cycle, Some("h23"));
        let f = DateTimeFormatter::new(&opts("en-US", "+05:30", ymd_hms())).unwrap();
        assert_eq!(f.format(t), "7/1/2024, 6:04:56\u{202f}PM");
        assert_eq!(default_hour_cycle("en-US"), "h12");
        assert_eq!(default_hour_cycle("ja-JP"), "h23");
    }

    #[test]
    fn parts_and_styles() {
        let t = 1_719_837_296_123.0;
        let mut c = ymd_hms();
        c.fractional_second_digits = Some(3);
        c.time_zone_name = Some("short".into());
        let f = DateTimeFormatter::new(&opts("en-US", "America/New_York", c)).unwrap();
        let parts = f.format_to_parts(t);
        let kinds: Vec<&str> = parts.iter().map(|p| p.kind).collect();
        assert!(kinds.contains(&"fractionalSecond"), "{parts:?}");
        assert!(kinds.contains(&"timeZoneName"), "{parts:?}");
        assert_eq!(f.format(t), "7/1/2024, 8:34:56.123\u{202f}AM EDT");
        let o = DateTimeFormatOptions {
            locale: "en-US".into(),
            time_zone: "UTC".into(),
            components: Components::default(),
            date_style: Some("full".into()),
            time_style: Some("short".into()),
        };
        let f = DateTimeFormatter::new(&o).unwrap();
        assert_eq!(f.format(t), "Monday, July 1, 2024 at 12:34\u{202f}PM");
        let o = DateTimeFormatOptions {
            locale: "en-US-u-ca-japanese".into(),
            time_zone: "UTC".into(),
            components: Components { year: Some("numeric".into()), era: Some("short".into()), ..Default::default() },
            date_style: None,
            time_style: None,
        };
        let f = DateTimeFormatter::new(&o).unwrap();
        assert!(f.format(t).starts_with("6 R"), "{}", f.format(t));
    }

    #[test]
    fn ranges() {
        let a = 1_719_837_296_000.0;
        let b = a + 3.0 * 86_400_000.0;
        let c = Components {
            year: Some("numeric".into()),
            month: Some("short".into()),
            day: Some("numeric".into()),
            ..Default::default()
        };
        let f = DateTimeFormatter::new(&opts("en-US", "UTC", c)).unwrap();
        let s = f.format_range(a, b);
        assert_eq!(s, "Jul 1 – 4, 2024", "{s}");
        let parts = f.format_range_to_parts(a, b);
        assert!(parts.iter().any(|p| p.source == "startRange"));
        assert!(parts.iter().any(|p| p.source == "endRange"));
    }
}
