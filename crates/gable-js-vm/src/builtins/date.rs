//! Date (ES2025 §21.4) plus the Annex B `getYear`, `setYear` and
//! `toGMTString`. Time values are UTC milliseconds in an `f64`; the clock
//! and the local time zone come from the host (`Host::now_ms`,
//! `Host::time_zone_offset_ms`, `Host::time_zone_name`), so the engine
//! itself knows no platform and defaults to UTC.

use gable_engine_api::RealmId;

use crate::convert::Hint;
use crate::object::{CallArgs, Class, ObjectData};
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, PropKey, Value};
use crate::vm::{JsResult, Vm};

const MS_PER_SECOND: f64 = 1000.0;
const MS_PER_MINUTE: f64 = 60_000.0;
const MS_PER_HOUR: f64 = 3_600_000.0;
const MS_PER_DAY: f64 = 86_400_000.0;
const MAX_TIME: f64 = 8.64e15;

// ---- time arithmetic (§21.4.1) ------------------------------------------------------------

fn pos_mod(a: f64, b: f64) -> f64 {
    let r = a % b;
    if r < 0.0 {
        r + b
    } else {
        r
    }
}

fn day(t: f64) -> f64 {
    (t / MS_PER_DAY).floor()
}

fn time_within_day(t: f64) -> f64 {
    pos_mod(t, MS_PER_DAY)
}

fn days_in_year(y: f64) -> f64 {
    if y % 4.0 != 0.0 {
        365.0
    } else if y % 100.0 != 0.0 {
        366.0
    } else if y % 400.0 != 0.0 {
        365.0
    } else {
        366.0
    }
}

fn day_from_year(y: f64) -> f64 {
    365.0 * (y - 1970.0) + ((y - 1969.0) / 4.0).floor() - ((y - 1901.0) / 100.0).floor()
        + ((y - 1601.0) / 400.0).floor()
}

fn time_from_year(y: f64) -> f64 {
    MS_PER_DAY * day_from_year(y)
}

fn year_from_time(t: f64) -> f64 {
    let mut y = (t / (MS_PER_DAY * 365.2425)).floor() + 1970.0;
    while time_from_year(y) > t {
        y -= 1.0;
    }
    while time_from_year(y + 1.0) <= t {
        y += 1.0;
    }
    y
}

fn in_leap_year(t: f64) -> bool {
    days_in_year(year_from_time(t)) == 366.0
}

fn day_within_year(t: f64) -> f64 {
    day(t) - day_from_year(year_from_time(t))
}

/// Cumulative days before each month, non-leap and leap.
const MONTH_START: [[f64; 13]; 2] = [
    [0.0, 31.0, 59.0, 90.0, 120.0, 151.0, 181.0, 212.0, 243.0, 273.0, 304.0, 334.0, 365.0],
    [0.0, 31.0, 60.0, 91.0, 121.0, 152.0, 182.0, 213.0, 244.0, 274.0, 305.0, 335.0, 366.0],
];

fn month_from_time(t: f64) -> f64 {
    let d = day_within_year(t);
    let leap = in_leap_year(t) as usize;
    for m in 0..12 {
        if d < MONTH_START[leap][m + 1] {
            return m as f64;
        }
    }
    11.0
}

fn date_from_time(t: f64) -> f64 {
    let d = day_within_year(t);
    let leap = in_leap_year(t) as usize;
    let m = month_from_time(t) as usize;
    d - MONTH_START[leap][m] + 1.0
}

fn week_day(t: f64) -> f64 {
    pos_mod(day(t) + 4.0, 7.0)
}

fn hour_from_time(t: f64) -> f64 {
    pos_mod((t / MS_PER_HOUR).floor(), 24.0)
}

fn min_from_time(t: f64) -> f64 {
    pos_mod((t / MS_PER_MINUTE).floor(), 60.0)
}

fn sec_from_time(t: f64) -> f64 {
    pos_mod((t / MS_PER_SECOND).floor(), 60.0)
}

fn ms_from_time(t: f64) -> f64 {
    pos_mod(t, MS_PER_SECOND)
}

/// MakeTime(hour, min, sec, ms).
fn make_time(hour: f64, min: f64, sec: f64, ms: f64) -> f64 {
    if !hour.is_finite() || !min.is_finite() || !sec.is_finite() || !ms.is_finite() {
        return f64::NAN;
    }
    let h = hour.trunc();
    let m = min.trunc();
    let s = sec.trunc();
    let milli = ms.trunc();
    h * MS_PER_HOUR + m * MS_PER_MINUTE + s * MS_PER_SECOND + milli
}

/// MakeDay(year, month, date).
fn make_day(year: f64, month: f64, date: f64) -> f64 {
    if !year.is_finite() || !month.is_finite() || !date.is_finite() {
        return f64::NAN;
    }
    let y = year.trunc();
    let m = month.trunc();
    let dt = date.trunc();
    let ym = y + (m / 12.0).floor();
    if !ym.is_finite() || ym.abs() > 400_000.0 {
        return f64::NAN;
    }
    let mn = pos_mod(m, 12.0) as usize;
    let leap = (days_in_year(ym) == 366.0) as usize;
    let day_of_year_start = day_from_year(ym) + MONTH_START[leap][mn];
    day_of_year_start + dt - 1.0
}

/// MakeDate(day, time).
fn make_date(day: f64, time: f64) -> f64 {
    if !day.is_finite() || !time.is_finite() {
        return f64::NAN;
    }
    let tv = day * MS_PER_DAY + time;
    if !tv.is_finite() {
        return f64::NAN;
    }
    tv
}

/// MakeFullYear(year).
fn make_full_year(year: f64) -> f64 {
    if year.is_nan() {
        return f64::NAN;
    }
    let truncated = year.trunc();
    if (0.0..=99.0).contains(&truncated) {
        1900.0 + truncated
    } else {
        truncated
    }
}

/// TimeClip(time).
fn time_clip(time: f64) -> f64 {
    if !time.is_finite() || time.abs() > MAX_TIME {
        return f64::NAN;
    }
    let t = time.trunc();
    if t == 0.0 {
        0.0
    } else {
        t
    }
}

fn local_time(vm: &Vm, t: f64) -> f64 {
    if t.is_nan() {
        return t;
    }
    t + vm.host.time_zone_offset_ms(t, true)
}

fn utc(vm: &Vm, t: f64) -> f64 {
    if !t.is_finite() {
        return f64::NAN;
    }
    t - vm.host.time_zone_offset_ms(t, false)
}

// ---- formatting --------------------------------------------------------------------------

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn year_string(y: f64) -> String {
    let sign = if y < 0.0 { "-" } else { "" };
    format!("{sign}{:04}", y.abs() as i64)
}

/// DateString(tv) for a local time value.
fn date_string(t: f64) -> String {
    format!(
        "{} {} {:02} {}",
        WEEKDAYS[week_day(t) as usize],
        MONTHS[month_from_time(t) as usize],
        date_from_time(t) as i64,
        year_string(year_from_time(t))
    )
}

/// TimeString(tv).
fn time_string(t: f64) -> String {
    format!(
        "{:02}:{:02}:{:02} GMT",
        hour_from_time(t) as i64,
        min_from_time(t) as i64,
        sec_from_time(t) as i64
    )
}

/// TimeZoneString(tv) for a UTC time value.
fn time_zone_string(vm: &Vm, tv: f64) -> String {
    let offset = vm.host.time_zone_offset_ms(tv, true);
    let sign = if offset >= 0.0 { "+" } else { "-" };
    let abs = offset.abs();
    let h = (abs / MS_PER_HOUR).floor();
    let m = ((abs % MS_PER_HOUR) / MS_PER_MINUTE).floor();
    format!(
        "{sign}{:02}{:02} ({})",
        h as i64,
        m as i64,
        vm.host.time_zone_name(tv)
    )
}

/// ToDateString(tv).
fn to_date_string(vm: &Vm, tv: f64) -> String {
    if tv.is_nan() {
        return "Invalid Date".into();
    }
    let t = local_time(vm, tv);
    format!("{} {}{}", date_string(t), time_string(t), time_zone_string(vm, tv))
}

fn to_utc_string(tv: f64) -> String {
    if tv.is_nan() {
        return "Invalid Date".into();
    }
    format!(
        "{}, {:02} {} {} {}",
        WEEKDAYS[week_day(tv) as usize],
        date_from_time(tv) as i64,
        MONTHS[month_from_time(tv) as usize],
        year_string(year_from_time(tv)),
        time_string(tv)
    )
}

fn to_iso_string(tv: f64) -> String {
    let y = year_from_time(tv);
    let year = if (0.0..=9999.0).contains(&y) {
        format!("{:04}", y as i64)
    } else {
        let sign = if y < 0.0 { "-" } else { "+" };
        format!("{sign}{:06}", y.abs() as i64)
    };
    format!(
        "{year}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        month_from_time(tv) as i64 + 1,
        date_from_time(tv) as i64,
        hour_from_time(tv) as i64,
        min_from_time(tv) as i64,
        sec_from_time(tv) as i64,
        ms_from_time(tv) as i64
    )
}

// ---- parsing -----------------------------------------------------------------------------

/// Date.parse: the Date Time String Format (§21.4.1.32) first, then the
/// `toString` and `toUTCString` shapes and a lenient fallback.
fn parse_date(vm: &Vm, s: &str) -> f64 {
    let s = s.trim();
    if let Some(t) = parse_iso(vm, s) {
        return t;
    }
    parse_lenient(vm, s)
}

struct Cursor<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Cursor<'a> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }
    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn digits(&mut self, n: usize) -> Option<u32> {
        let mut v: u32 = 0;
        for k in 0..n {
            let c = *self.b.get(self.i + k)?;
            if !c.is_ascii_digit() {
                return None;
            }
            v = v * 10 + (c - b'0') as u32;
        }
        self.i += n;
        Some(v)
    }
    fn done(&self) -> bool {
        self.i >= self.b.len()
    }
}

fn parse_iso(vm: &Vm, s: &str) -> Option<f64> {
    let mut c = Cursor { b: s.as_bytes(), i: 0 };
    // Year: YYYY or ±YYYYYY.
    let year: f64 = match c.peek()? {
        b'+' | b'-' => {
            let neg = c.peek() == Some(b'-');
            c.i += 1;
            let y = c.digits(6)?;
            if neg && y == 0 {
                return None;
            }
            if neg {
                -(y as f64)
            } else {
                y as f64
            }
        }
        _ => c.digits(4)? as f64,
    };
    let mut month = 1u32;
    let mut dayv = 1u32;
    if c.eat(b'-') {
        month = c.digits(2)?;
        if c.eat(b'-') {
            dayv = c.digits(2)?;
        }
    }
    if !(1..=12).contains(&month) {
        return None;
    }
    let leap = (days_in_year(year) == 366.0) as usize;
    let dim = MONTH_START[leap][month as usize] - MONTH_START[leap][month as usize - 1];
    if dayv < 1 || dayv as f64 > dim {
        return None;
    }
    let mut hour = 0u32;
    let mut minute = 0u32;
    let mut second = 0u32;
    let mut milli = 0u32;
    let mut has_time = false;
    let mut offset: Option<f64> = None;
    if c.eat(b'T') || c.eat(b't') {
        has_time = true;
        hour = c.digits(2)?;
        if !c.eat(b':') {
            return None;
        }
        minute = c.digits(2)?;
        if c.eat(b':') {
            second = c.digits(2)?;
            if c.eat(b'.') || c.eat(b',') {
                // At least one digit; the first three count.
                let start = c.i;
                while c.peek().map(|d| d.is_ascii_digit()).unwrap_or(false) {
                    c.i += 1;
                }
                if c.i == start {
                    return None;
                }
                let frac = &s[start..c.i];
                let mut ms = 0u32;
                for k in 0..3 {
                    let d = frac.as_bytes().get(k).map(|d| (d - b'0') as u32).unwrap_or(0);
                    ms = ms * 10 + d;
                }
                milli = ms;
            }
        }
        match c.peek() {
            Some(b'Z') | Some(b'z') => {
                c.i += 1;
                offset = Some(0.0);
            }
            Some(b'+') | Some(b'-') => {
                let neg = c.peek() == Some(b'-');
                c.i += 1;
                let oh = c.digits(2)?;
                if !c.eat(b':') {
                    return None;
                }
                let om = c.digits(2)?;
                if oh > 23 || om > 59 {
                    return None;
                }
                let o = (oh as f64) * MS_PER_HOUR + (om as f64) * MS_PER_MINUTE;
                offset = Some(if neg { -o } else { o });
            }
            _ => {}
        }
    }
    if !c.done() {
        return None;
    }
    if hour > 24 || minute > 59 || second > 59 {
        return None;
    }
    if hour == 24 && (minute != 0 || second != 0 || milli != 0) {
        return None;
    }
    let dayn = make_day(year, (month - 1) as f64, dayv as f64);
    let time = make_time(hour as f64, minute as f64, second as f64, milli as f64);
    let mut t = make_date(dayn, time);
    match offset {
        Some(o) => t -= o,
        None => {
            if has_time {
                t = utc(vm, t);
            }
        }
    }
    Some(time_clip(t))
}

/// The `toString`/`toUTCString` shapes and common variants: tokens of
/// weekday names (ignored), month names, numbers, `hh:mm[:ss[.mmm]]`,
/// `GMT`/`UTC`/`Z` with an optional `±hh[:]mm`, and parenthesised text.
fn parse_lenient(vm: &Vm, s: &str) -> f64 {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut year: Option<f64> = None;
    let mut month: Option<f64> = None;
    let mut dayv: Option<f64> = None;
    let mut hour = 0.0;
    let mut minute = 0.0;
    let mut second = 0.0;
    let mut milli = 0.0;
    let mut offset: Option<f64> = None;
    let mut numbers: Vec<f64> = Vec::new();
    let mut pm: Option<bool> = None;
    while i < bytes.len() {
        let ch = bytes[i];
        if ch == b'(' {
            let mut depth = 1;
            i += 1;
            while i < bytes.len() && depth > 0 {
                if bytes[i] == b'(' {
                    depth += 1;
                } else if bytes[i] == b')' {
                    depth -= 1;
                }
                i += 1;
            }
            continue;
        }
        if ch.is_ascii_alphabetic() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            let word = s[start..i].to_ascii_lowercase();
            if word == "gmt" || word == "utc" || word == "ut" || word == "z" {
                offset.get_or_insert(0.0);
                // Optional ±hh[:]mm directly after.
                if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
                    let neg = bytes[i] == b'-';
                    i += 1;
                    let ds = i;
                    while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b':') {
                        i += 1;
                    }
                    let digits: String = s[ds..i].chars().filter(|c| c.is_ascii_digit()).collect();
                    let (h, m) = match digits.len() {
                        4 => (digits[..2].parse::<f64>().unwrap_or(0.0), digits[2..].parse::<f64>().unwrap_or(0.0)),
                        1 | 2 => (digits.parse::<f64>().unwrap_or(0.0), 0.0),
                        3 => (digits[..1].parse::<f64>().unwrap_or(0.0), digits[1..].parse::<f64>().unwrap_or(0.0)),
                        _ => return f64::NAN,
                    };
                    let o = h * MS_PER_HOUR + m * MS_PER_MINUTE;
                    offset = Some(if neg { -o } else { o });
                }
                continue;
            }
            if word == "am" {
                pm = Some(false);
                continue;
            }
            if word == "pm" {
                pm = Some(true);
                continue;
            }
            if let Some(m) = MONTHS.iter().position(|m| word.starts_with(&m.to_ascii_lowercase())) {
                if word.len() >= 3 {
                    month = Some(m as f64);
                    continue;
                }
            }
            if WEEKDAYS.iter().any(|w| word.starts_with(&w.to_ascii_lowercase())) && word.len() >= 3 {
                continue;
            }
            if word == "t" {
                continue;
            }
            return f64::NAN;
        }
        if ch.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let n: f64 = s[start..i].parse().unwrap_or(f64::NAN);
            if i < bytes.len() && bytes[i] == b':' {
                // Time.
                hour = n;
                i += 1;
                let ms = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                minute = s[ms..i].parse().unwrap_or(f64::NAN);
                if i < bytes.len() && bytes[i] == b':' {
                    i += 1;
                    let ss = i;
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                    second = s[ss..i].parse().unwrap_or(f64::NAN);
                    if i < bytes.len() && bytes[i] == b'.' {
                        i += 1;
                        let fs = i;
                        while i < bytes.len() && bytes[i].is_ascii_digit() {
                            i += 1;
                        }
                        let frac = &s[fs..i];
                        let mut ms = 0.0;
                        for k in 0..3 {
                            let d = frac.as_bytes().get(k).map(|d| (d - b'0') as f64).unwrap_or(0.0);
                            ms = ms * 10.0 + d;
                        }
                        milli = ms;
                    }
                }
                continue;
            }
            if i < bytes.len() && (bytes[i] == b'/' || bytes[i] == b'-') && month.is_none() && numbers.is_empty() {
                // m/d/y or y-m-d (non-ISO)
                let sep = bytes[i];
                let mut parts = vec![n];
                while i < bytes.len() && bytes[i] == sep {
                    i += 1;
                    let ps = i;
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                    if ps == i {
                        return f64::NAN;
                    }
                    parts.push(s[ps..i].parse().unwrap_or(f64::NAN));
                }
                if parts.len() != 3 {
                    return f64::NAN;
                }
                if parts[0] > 31.0 {
                    year = Some(parts[0]);
                    month = Some(parts[1] - 1.0);
                    dayv = Some(parts[2]);
                } else {
                    month = Some(parts[0] - 1.0);
                    dayv = Some(parts[1]);
                    year = Some(parts[2]);
                }
                continue;
            }
            numbers.push(n);
            continue;
        }
        if ch == b'+' || ch == b'-' {
            // A signed year or a bare offset after the time.
            let neg = ch == b'-';
            i += 1;
            let ds = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if ds == i {
                return f64::NAN;
            }
            let digits = &s[ds..i];
            if offset.is_some() || (digits.len() == 4 && hour != 0.0 || minute != 0.0) && year.is_some() {
                let h: f64 = digits[..digits.len().saturating_sub(2)].parse().unwrap_or(0.0);
                let m: f64 = digits[digits.len().saturating_sub(2)..].parse().unwrap_or(0.0);
                let o = h * MS_PER_HOUR + m * MS_PER_MINUTE;
                offset = Some(if neg { -o } else { o });
            } else {
                let n: f64 = digits.parse().unwrap_or(f64::NAN);
                year = Some(if neg { -n } else { n });
            }
            continue;
        }
        if ch == b' ' || ch == b',' || ch == b'.' || ch == b'/' {
            i += 1;
            continue;
        }
        return f64::NAN;
    }
    // Assign the loose numbers: day (1..31) and year.
    for n in numbers {
        if dayv.is_none() && (1.0..=31.0).contains(&n) && month.is_some() {
            dayv = Some(n);
        } else if year.is_none() {
            year = Some(n);
        } else if dayv.is_none() {
            dayv = Some(n);
        } else {
            return f64::NAN;
        }
    }
    let (Some(y), Some(m)) = (year, month) else {
        return f64::NAN;
    };
    let d = dayv.unwrap_or(1.0);
    let y = if (0.0..=49.0).contains(&y) {
        2000.0 + y
    } else if (50.0..=99.0).contains(&y) {
        1900.0 + y
    } else {
        y
    };
    if let Some(is_pm) = pm {
        if is_pm && hour < 12.0 {
            hour += 12.0;
        } else if !is_pm && hour == 12.0 {
            hour = 0.0;
        }
    }
    let t = make_date(make_day(y, m, d), make_time(hour, minute, second, milli));
    let t = match offset {
        Some(o) => t - o,
        None => utc(vm, t),
    };
    time_clip(t)
}

// ---- install -----------------------------------------------------------------------------

pub fn install(vm: &mut Vm, realm: RealmId) {
    let proto = vm.new_object();
    let ctor = super::install_ctor(vm, realm, "Date", 7, date_constructor, proto, Intrinsic::Date, Intrinsic::DatePrototype);
    vm.method(ctor, "now", 0, date_now);
    vm.method(ctor, "parse", 1, date_parse);
    vm.method(ctor, "UTC", 7, date_utc);

    vm.method(proto, "getDate", 0, get_date);
    vm.method(proto, "getDay", 0, get_day);
    vm.method(proto, "getFullYear", 0, get_full_year);
    vm.method(proto, "getHours", 0, get_hours);
    vm.method(proto, "getMilliseconds", 0, get_milliseconds);
    vm.method(proto, "getMinutes", 0, get_minutes);
    vm.method(proto, "getMonth", 0, get_month);
    vm.method(proto, "getSeconds", 0, get_seconds);
    vm.method(proto, "getTime", 0, get_time);
    vm.method(proto, "getTimezoneOffset", 0, get_timezone_offset);
    vm.method(proto, "getUTCDate", 0, get_utc_date);
    vm.method(proto, "getUTCDay", 0, get_utc_day);
    vm.method(proto, "getUTCFullYear", 0, get_utc_full_year);
    vm.method(proto, "getUTCHours", 0, get_utc_hours);
    vm.method(proto, "getUTCMilliseconds", 0, get_utc_milliseconds);
    vm.method(proto, "getUTCMinutes", 0, get_utc_minutes);
    vm.method(proto, "getUTCMonth", 0, get_utc_month);
    vm.method(proto, "getUTCSeconds", 0, get_utc_seconds);
    vm.method(proto, "getYear", 0, get_year);
    vm.method(proto, "setDate", 1, set_date);
    vm.method(proto, "setFullYear", 3, set_full_year);
    vm.method(proto, "setHours", 4, set_hours);
    vm.method(proto, "setMilliseconds", 1, set_milliseconds);
    vm.method(proto, "setMinutes", 3, set_minutes);
    vm.method(proto, "setMonth", 2, set_month);
    vm.method(proto, "setSeconds", 2, set_seconds);
    vm.method(proto, "setTime", 1, set_time);
    vm.method(proto, "setUTCDate", 1, set_utc_date);
    vm.method(proto, "setUTCFullYear", 3, set_utc_full_year);
    vm.method(proto, "setUTCHours", 4, set_utc_hours);
    vm.method(proto, "setUTCMilliseconds", 1, set_utc_milliseconds);
    vm.method(proto, "setUTCMinutes", 3, set_utc_minutes);
    vm.method(proto, "setUTCMonth", 2, set_utc_month);
    vm.method(proto, "setUTCSeconds", 2, set_utc_seconds);
    vm.method(proto, "setYear", 1, set_year);
    vm.method(proto, "toDateString", 0, to_date_string_m);
    vm.method(proto, "toISOString", 0, to_iso_string_m);
    vm.method(proto, "toJSON", 1, to_json);
    vm.method(proto, "toLocaleDateString", 0, to_date_string_m);
    vm.method(proto, "toLocaleString", 0, to_string_m);
    vm.method(proto, "toLocaleTimeString", 0, to_time_string_m);
    vm.method(proto, "toString", 0, to_string_m);
    vm.method(proto, "toTimeString", 0, to_time_string_m);
    let utc_s = vm.method(proto, "toUTCString", 0, to_utc_string_m);
    vm.define_raw_str(proto, "toGMTString", Value::Object(utc_s), Attrs::METHOD);
    vm.method(proto, "valueOf", 0, get_time);
    let tp = vm.well_known.to_primitive;
    let f = vm.method_sym(proto, tp, "Symbol.toPrimitive", 1, to_primitive);
    // @@toPrimitive is non-writable.
    let key = PropKey::Sym(tp);
    vm.define_raw(proto, key, Value::Object(f), Attrs::CONFIGURABLE_ONLY);
}

// ---- constructor and statics -------------------------------------------------------------

fn this_time_value(vm: &mut Vm, this: Value, method: &str) -> JsResult<(ObjId, f64)> {
    if let Value::Object(o) = this {
        if let Class::Date(t) = &vm.heap.obj(o).class {
            return Ok((o, *t));
        }
    }
    vm.type_error(&format!("Date.prototype.{method} called on incompatible receiver"))
}

fn set_date_value(vm: &mut Vm, o: ObjId, t: f64) {
    vm.heap.obj_mut(o).class = Class::Date(t);
}

fn date_constructor(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    if a.new_target.is_undefined() {
        let now = vm.host.now_ms();
        let s = to_date_string(vm, now);
        return Ok(vm.str(&s));
    }
    let n = a.args.len();
    let dv = if n == 0 {
        vm.host.now_ms()
    } else if n == 1 {
        let value = a.arg(0);
        let tv = if let Value::Object(o) = value {
            if let Class::Date(t) = &vm.heap.obj(o).class {
                *t
            } else {
                let v = vm.to_primitive(value, Hint::Default)?;
                prim_to_time(vm, v)?
            }
        } else {
            prim_to_time(vm, value)?
        };
        time_clip(tv)
    } else {
        let y = vm.to_number(a.arg(0))?;
        let m = vm.to_number(a.arg(1))?;
        let dt = if n > 2 { vm.to_number(a.arg(2))? } else { 1.0 };
        let h = if n > 3 { vm.to_number(a.arg(3))? } else { 0.0 };
        let min = if n > 4 { vm.to_number(a.arg(4))? } else { 0.0 };
        let s = if n > 5 { vm.to_number(a.arg(5))? } else { 0.0 };
        let milli = if n > 6 { vm.to_number(a.arg(6))? } else { 0.0 };
        let yr = make_full_year(y);
        let final_date = make_date(make_day(yr, m, dt), make_time(h, min, s, milli));
        time_clip(utc(vm, final_date))
    };
    let proto = vm.get_prototype_from_constructor(a.new_target, Intrinsic::DatePrototype)?;
    Ok(Value::Object(vm.alloc(ObjectData::new(Some(proto), Class::Date(dv)))))
}

fn prim_to_time(vm: &mut Vm, v: Value) -> JsResult<f64> {
    if let Value::String(s) = v {
        let text = vm.heap.units(s).to_string_lossy();
        Ok(parse_date(vm, &text))
    } else {
        vm.to_number(v)
    }
}

fn date_now(vm: &mut Vm, _a: &CallArgs) -> JsResult<Value> {
    Ok(Value::Number(vm.host.now_ms()))
}

fn date_parse(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let s = vm.to_string(a.arg(0))?;
    let text = vm.heap.units(s).to_string_lossy();
    Ok(Value::Number(parse_date(vm, &text)))
}

fn date_utc(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let n = a.args.len();
    let y = vm.to_number(a.arg(0))?;
    let m = if n > 1 { vm.to_number(a.arg(1))? } else { 0.0 };
    let dt = if n > 2 { vm.to_number(a.arg(2))? } else { 1.0 };
    let h = if n > 3 { vm.to_number(a.arg(3))? } else { 0.0 };
    let min = if n > 4 { vm.to_number(a.arg(4))? } else { 0.0 };
    let s = if n > 5 { vm.to_number(a.arg(5))? } else { 0.0 };
    let milli = if n > 6 { vm.to_number(a.arg(6))? } else { 0.0 };
    let yr = make_full_year(y);
    Ok(Value::Number(time_clip(make_date(
        make_day(yr, m, dt),
        make_time(h, min, s, milli),
    ))))
}

// ---- getters -----------------------------------------------------------------------------

fn getter(vm: &mut Vm, a: &CallArgs, name: &str, local: bool, f: fn(f64) -> f64) -> JsResult<Value> {
    let (_, t) = this_time_value(vm, a.this, name)?;
    if t.is_nan() {
        return Ok(Value::Number(f64::NAN));
    }
    let t = if local { local_time(vm, t) } else { t };
    Ok(Value::Number(f(t)))
}

fn get_date(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    getter(vm, a, "getDate", true, date_from_time)
}
fn get_day(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    getter(vm, a, "getDay", true, week_day)
}
fn get_full_year(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    getter(vm, a, "getFullYear", true, year_from_time)
}
fn get_hours(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    getter(vm, a, "getHours", true, hour_from_time)
}
fn get_milliseconds(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    getter(vm, a, "getMilliseconds", true, ms_from_time)
}
fn get_minutes(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    getter(vm, a, "getMinutes", true, min_from_time)
}
fn get_month(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    getter(vm, a, "getMonth", true, month_from_time)
}
fn get_seconds(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    getter(vm, a, "getSeconds", true, sec_from_time)
}
fn get_time(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (_, t) = this_time_value(vm, a.this, "getTime")?;
    Ok(Value::Number(t))
}
fn get_timezone_offset(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (_, t) = this_time_value(vm, a.this, "getTimezoneOffset")?;
    if t.is_nan() {
        return Ok(Value::Number(f64::NAN));
    }
    Ok(Value::Number((t - local_time(vm, t)) / MS_PER_MINUTE))
}
fn get_utc_date(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    getter(vm, a, "getUTCDate", false, date_from_time)
}
fn get_utc_day(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    getter(vm, a, "getUTCDay", false, week_day)
}
fn get_utc_full_year(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    getter(vm, a, "getUTCFullYear", false, year_from_time)
}
fn get_utc_hours(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    getter(vm, a, "getUTCHours", false, hour_from_time)
}
fn get_utc_milliseconds(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    getter(vm, a, "getUTCMilliseconds", false, ms_from_time)
}
fn get_utc_minutes(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    getter(vm, a, "getUTCMinutes", false, min_from_time)
}
fn get_utc_month(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    getter(vm, a, "getUTCMonth", false, month_from_time)
}
fn get_utc_seconds(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    getter(vm, a, "getUTCSeconds", false, sec_from_time)
}
fn get_year(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    getter(vm, a, "getYear", true, |t| year_from_time(t) - 1900.0)
}

// ---- setters -----------------------------------------------------------------------------

/// Coerce the present arguments in order (the spec converts every argument
/// before checking the time value).
fn numbers(vm: &mut Vm, a: &CallArgs, count: usize) -> JsResult<Vec<Option<f64>>> {
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        if i < a.args.len() {
            out.push(Some(vm.to_number(a.arg(i))?));
        } else {
            out.push(None);
        }
    }
    Ok(out)
}

fn finish(vm: &mut Vm, o: ObjId, local: bool, date: f64) -> JsResult<Value> {
    let u = if local { time_clip(utc(vm, date)) } else { time_clip(date) };
    set_date_value(vm, o, u);
    Ok(Value::Number(u))
}

fn set_date_impl(vm: &mut Vm, a: &CallArgs, name: &str, local: bool) -> JsResult<Value> {
    let (o, t) = this_time_value(vm, a.this, name)?;
    let dt = vm.to_number(a.arg(0))?;
    if t.is_nan() {
        return Ok(Value::Number(f64::NAN));
    }
    let t = if local { local_time(vm, t) } else { t };
    let new_date = make_date(make_day(year_from_time(t), month_from_time(t), dt), time_within_day(t));
    finish(vm, o, local, new_date)
}
fn set_date(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    set_date_impl(vm, a, "setDate", true)
}
fn set_utc_date(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    set_date_impl(vm, a, "setUTCDate", false)
}

fn set_full_year_impl(vm: &mut Vm, a: &CallArgs, name: &str, local: bool) -> JsResult<Value> {
    let (o, t) = this_time_value(vm, a.this, name)?;
    let y = vm.to_number(a.arg(0))?;
    let t = if t.is_nan() {
        0.0
    } else if local {
        local_time(vm, t)
    } else {
        t
    };
    let m = if a.args.len() > 1 { vm.to_number(a.arg(1))? } else { month_from_time(t) };
    let dt = if a.args.len() > 2 { vm.to_number(a.arg(2))? } else { date_from_time(t) };
    let new_date = make_date(make_day(y, m, dt), time_within_day(t));
    finish(vm, o, local, new_date)
}
fn set_full_year(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    set_full_year_impl(vm, a, "setFullYear", true)
}
fn set_utc_full_year(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    set_full_year_impl(vm, a, "setUTCFullYear", false)
}

fn set_hours_impl(vm: &mut Vm, a: &CallArgs, name: &str, local: bool) -> JsResult<Value> {
    let (o, t) = this_time_value(vm, a.this, name)?;
    let n = numbers(vm, a, 4)?;
    let h = n[0].unwrap_or(f64::NAN);
    if t.is_nan() {
        return Ok(Value::Number(f64::NAN));
    }
    let t = if local { local_time(vm, t) } else { t };
    let m = n[1].unwrap_or_else(|| min_from_time(t));
    let s = n[2].unwrap_or_else(|| sec_from_time(t));
    let milli = n[3].unwrap_or_else(|| ms_from_time(t));
    let date = make_date(day(t), make_time(h, m, s, milli));
    finish(vm, o, local, date)
}
fn set_hours(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    set_hours_impl(vm, a, "setHours", true)
}
fn set_utc_hours(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    set_hours_impl(vm, a, "setUTCHours", false)
}

fn set_milliseconds_impl(vm: &mut Vm, a: &CallArgs, name: &str, local: bool) -> JsResult<Value> {
    let (o, t) = this_time_value(vm, a.this, name)?;
    let ms = vm.to_number(a.arg(0))?;
    if t.is_nan() {
        return Ok(Value::Number(f64::NAN));
    }
    let t = if local { local_time(vm, t) } else { t };
    let time = make_time(hour_from_time(t), min_from_time(t), sec_from_time(t), ms);
    finish(vm, o, local, make_date(day(t), time))
}
fn set_milliseconds(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    set_milliseconds_impl(vm, a, "setMilliseconds", true)
}
fn set_utc_milliseconds(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    set_milliseconds_impl(vm, a, "setUTCMilliseconds", false)
}

fn set_minutes_impl(vm: &mut Vm, a: &CallArgs, name: &str, local: bool) -> JsResult<Value> {
    let (o, t) = this_time_value(vm, a.this, name)?;
    let n = numbers(vm, a, 3)?;
    let m = n[0].unwrap_or(f64::NAN);
    if t.is_nan() {
        return Ok(Value::Number(f64::NAN));
    }
    let t = if local { local_time(vm, t) } else { t };
    let s = n[1].unwrap_or_else(|| sec_from_time(t));
    let milli = n[2].unwrap_or_else(|| ms_from_time(t));
    let date = make_date(day(t), make_time(hour_from_time(t), m, s, milli));
    finish(vm, o, local, date)
}
fn set_minutes(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    set_minutes_impl(vm, a, "setMinutes", true)
}
fn set_utc_minutes(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    set_minutes_impl(vm, a, "setUTCMinutes", false)
}

fn set_month_impl(vm: &mut Vm, a: &CallArgs, name: &str, local: bool) -> JsResult<Value> {
    let (o, t) = this_time_value(vm, a.this, name)?;
    let n = numbers(vm, a, 2)?;
    let m = n[0].unwrap_or(f64::NAN);
    if t.is_nan() {
        return Ok(Value::Number(f64::NAN));
    }
    let t = if local { local_time(vm, t) } else { t };
    let dt = n[1].unwrap_or_else(|| date_from_time(t));
    let new_date = make_date(make_day(year_from_time(t), m, dt), time_within_day(t));
    finish(vm, o, local, new_date)
}
fn set_month(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    set_month_impl(vm, a, "setMonth", true)
}
fn set_utc_month(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    set_month_impl(vm, a, "setUTCMonth", false)
}

fn set_seconds_impl(vm: &mut Vm, a: &CallArgs, name: &str, local: bool) -> JsResult<Value> {
    let (o, t) = this_time_value(vm, a.this, name)?;
    let n = numbers(vm, a, 2)?;
    let s = n[0].unwrap_or(f64::NAN);
    if t.is_nan() {
        return Ok(Value::Number(f64::NAN));
    }
    let t = if local { local_time(vm, t) } else { t };
    let milli = n[1].unwrap_or_else(|| ms_from_time(t));
    let date = make_date(day(t), make_time(hour_from_time(t), min_from_time(t), s, milli));
    finish(vm, o, local, date)
}
fn set_seconds(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    set_seconds_impl(vm, a, "setSeconds", true)
}
fn set_utc_seconds(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    set_seconds_impl(vm, a, "setUTCSeconds", false)
}

fn set_time(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, _) = this_time_value(vm, a.this, "setTime")?;
    let t = vm.to_number(a.arg(0))?;
    let v = time_clip(t);
    set_date_value(vm, o, v);
    Ok(Value::Number(v))
}

fn set_year(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (o, t) = this_time_value(vm, a.this, "setYear")?;
    let y = vm.to_number(a.arg(0))?;
    let t = if t.is_nan() { 0.0 } else { local_time(vm, t) };
    let yyyy = make_full_year(y);
    let d = make_day(yyyy, month_from_time(t), date_from_time(t));
    let date = make_date(d, time_within_day(t));
    finish(vm, o, true, date)
}

// ---- strings -----------------------------------------------------------------------------

fn to_string_m(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (_, t) = this_time_value(vm, a.this, "toString")?;
    let s = to_date_string(vm, t);
    Ok(vm.str(&s))
}

fn to_date_string_m(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (_, t) = this_time_value(vm, a.this, "toDateString")?;
    if t.is_nan() {
        return Ok(vm.str("Invalid Date"));
    }
    let s = date_string(local_time(vm, t));
    Ok(vm.str(&s))
}

fn to_time_string_m(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (_, t) = this_time_value(vm, a.this, "toTimeString")?;
    if t.is_nan() {
        return Ok(vm.str("Invalid Date"));
    }
    let s = format!("{}{}", time_string(local_time(vm, t)), time_zone_string(vm, t));
    Ok(vm.str(&s))
}

fn to_utc_string_m(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (_, t) = this_time_value(vm, a.this, "toUTCString")?;
    let s = to_utc_string(t);
    Ok(vm.str(&s))
}

fn to_iso_string_m(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let (_, t) = this_time_value(vm, a.this, "toISOString")?;
    if !t.is_finite() {
        return vm.range_error("Invalid time value");
    }
    let s = to_iso_string(t);
    Ok(vm.str(&s))
}

fn to_json(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let o = vm.to_object(a.this)?;
    let tv = vm.to_primitive(Value::Object(o), Hint::Number)?;
    if let Value::Number(n) = tv {
        if !n.is_finite() {
            return Ok(Value::Null);
        }
    }
    let key = PropKey::Str(vm.names.toISOString);
    vm.invoke(Value::Object(o), key, &[])
}

fn to_primitive(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Value::Object(o) = a.this else {
        return vm.type_error("Date.prototype[Symbol.toPrimitive] called on a non-object");
    };
    let hint = a.arg(0);
    let try_first = match hint {
        Value::String(s) => {
            if vm.heap.str_eq_ascii(s, "string") || vm.heap.str_eq_ascii(s, "default") {
                Hint::String
            } else if vm.heap.str_eq_ascii(s, "number") {
                Hint::Number
            } else {
                return vm.type_error("Invalid hint");
            }
        }
        _ => return vm.type_error("Invalid hint"),
    };
    vm.ordinary_to_primitive(o, try_first)
}
