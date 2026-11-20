#![allow(
    clippy::missing_errors_doc,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use crate::error::{ErrorCode, LiliaError, Result};
use serde::{Deserialize, Serialize};

pub const BITEMPORAL_INFINITY: i64 = i64::MAX;
pub const BITEMPORAL_NEG_INFINITY: i64 = i64::MIN;

/// Howard Hinnant's algorithm: Gregorian calendar calculation (days since 1970-01-01).
const fn days_from_civil(mut y: i32, m: u32, d: u32) -> i64 {
    if m <= 2 {
        y -= 1;
    }
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era as i64) * 146_097 + (doe as i64) - 719_468
}

/// Howard Hinnant's algorithm: Gregorian civil date from days since 1970-01-01.
const fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

/// Parses a date or timestamp string into UTC epoch milliseconds.
/// Supports:
/// - `YYYY-MM-DD` (e.g. "2026-11-20") -> start of that day UTC
/// - `YYYY-MM-DDTHH:MM:SSZ` or `YYYY-MM-DDTHH:MM:SS`
/// - `YYYY-MM-DDTHH:MM:SS.fffZ`
/// - `YYYY-MM-DDTHH:MM:SS+HH:MM` or `-HH:MM`
/// - Keywords: "infinity", "+infinity", "max", "-infinity", "min", "now"
/// - Numeric string representing epoch milliseconds (e.g. "1795132800000")
pub fn parse_iso_to_ms(s: &str) -> Result<i64> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            "cannot parse empty timestamp",
            false,
        ));
    }

    match trimmed.to_ascii_lowercase().as_str() {
        "infinity" | "+infinity" | "max" | "forever" => return Ok(BITEMPORAL_INFINITY),
        "-infinity" | "min" => return Ok(BITEMPORAL_NEG_INFINITY),
        "now" | "current" => {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as i64);
                return Ok(now);
            }
            #[cfg(target_arch = "wasm32")]
            {
                return Ok(js_sys::Date::now() as i64);
            }
        }
        _ => {}
    }

    // Direct numeric timestamp
    if let Ok(num) = trimmed.parse::<i64>() {
        return Ok(num);
    }

    // Format: YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS...
    let (date_part, time_part) = match trimmed.split_once('T').or_else(|| trimmed.split_once(' ')) {
        Some((d, t)) => (d, Some(t)),
        None => (trimmed, None),
    };

    let date_segs: Vec<&str> = date_part.split('-').collect();
    if date_segs.len() != 3 {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid date format (expected YYYY-MM-DD): {trimmed}"),
            false,
        ));
    }

    let year: i32 = date_segs[0].parse().map_err(|_| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid year: {}", date_segs[0]),
            false,
        )
    })?;
    let month: u32 = date_segs[1].parse().map_err(|_| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid month: {}", date_segs[1]),
            false,
        )
    })?;
    let day: u32 = date_segs[2].parse().map_err(|_| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid day: {}", date_segs[2]),
            false,
        )
    })?;

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            format!("date component out of range: {year}-{month:02}-{day:02}"),
            false,
        ));
    }

    let days = days_from_civil(year, month, day);
    let mut ms: i64 = days * 86_400_000;

    if let Some(mut time_str) = time_part {
        let mut tz_offset_ms: i64 = 0;
        if time_str.ends_with('Z') || time_str.ends_with('z') {
            time_str = &time_str[..time_str.len() - 1];
        } else if let Some(plus_idx) = time_str.rfind('+') {
            let offset_part = &time_str[plus_idx + 1..];
            tz_offset_ms = parse_tz_offset(offset_part)?;
            time_str = &time_str[..plus_idx];
        } else if let Some(minus_idx) = time_str.rfind('-') {
            let offset_part = &time_str[minus_idx + 1..];
            tz_offset_ms = -parse_tz_offset(offset_part)?;
            time_str = &time_str[..minus_idx];
        }

        let time_segs: Vec<&str> = time_str.split(':').collect();
        if time_segs.len() < 2 {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                format!("invalid time format: {time_str}"),
                false,
            ));
        }

        let hour: i64 = time_segs[0].parse().map_err(|_| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("invalid hour: {}", time_segs[0]),
                false,
            )
        })?;
        let min: i64 = time_segs[1].parse().map_err(|_| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("invalid minute: {}", time_segs[1]),
                false,
            )
        })?;

        let mut sec: i64 = 0;
        let mut millis: i64 = 0;

        if time_segs.len() >= 3 {
            let sec_str = time_segs[2];
            if let Some((s_part, ms_part)) = sec_str.split_once('.') {
                sec = s_part.parse().unwrap_or(0);
                let mut ms_num: i64 = ms_part.parse().unwrap_or(0);
                if ms_part.len() == 1 {
                    ms_num *= 100;
                } else if ms_part.len() == 2 {
                    ms_num *= 10;
                } else if ms_part.len() > 3 {
                    ms_num /= 10_i64.pow((ms_part.len() - 3) as u32);
                }
                millis = ms_num;
            } else {
                sec = sec_str.parse().unwrap_or(0);
            }
        }

        let daytime_ms = hour * 3_600_000 + min * 60_000 + sec * 1_000 + millis;
        ms += daytime_ms - tz_offset_ms;
    }

    Ok(ms)
}

fn parse_tz_offset(s: &str) -> Result<i64> {
    let parts: Vec<&str> = s.split(':').collect();
    let hours: i64 = parts[0].parse().map_err(|_| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid timezone offset: {s}"),
            false,
        )
    })?;
    let mins: i64 = if parts.len() > 1 {
        parts[1].parse().map_err(|_| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("invalid timezone offset: {s}"),
                false,
            )
        })?
    } else {
        0
    };
    Ok((hours * 60 + mins) * 60_000)
}

/// Formats UTC epoch milliseconds into ISO-8601 string: `YYYY-MM-DDTHH:MM:SS.fffZ`.
#[must_use]
pub fn format_ms_to_iso(ms: i64) -> String {
    if ms == BITEMPORAL_INFINITY {
        return "infinity".to_string();
    }
    if ms == BITEMPORAL_NEG_INFINITY {
        return "-infinity".to_string();
    }

    let days = ms.div_euclid(86_400_000);
    let rem_ms = ms.rem_euclid(86_400_000);

    let (year, month, day) = civil_from_days(days);
    let hour = rem_ms / 3_600_000;
    let minute = (rem_ms % 3_600_000) / 60_000;
    let second = (rem_ms % 60_000) / 1000;
    let millis = rem_ms % 1000;

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Formats UTC epoch milliseconds into date-only string: `YYYY-MM-DD`.
#[must_use]
pub fn format_ms_to_date(ms: i64) -> String {
    if ms == BITEMPORAL_INFINITY {
        return "infinity".to_string();
    }
    if ms == BITEMPORAL_NEG_INFINITY {
        return "-infinity".to_string();
    }

    let days = ms.div_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// A specific point in time on the bitemporal plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BitemporalPoint(pub i64);

impl BitemporalPoint {
    #[must_use]
    pub const fn from_ms(ms: i64) -> Self {
        Self(ms)
    }

    pub fn at(val: impl AsRef<str>) -> Result<Self> {
        let ms = parse_iso_to_ms(val.as_ref())?;
        Ok(Self(ms))
    }

    #[must_use]
    pub fn to_iso(self) -> String {
        format_ms_to_iso(self.0)
    }

    #[must_use]
    pub fn to_date(self) -> String {
        format_ms_to_date(self.0)
    }

    #[must_use]
    pub const fn as_ms(self) -> i64 {
        self.0
    }
}

impl From<i64> for BitemporalPoint {
    fn from(ms: i64) -> Self {
        Self(ms)
    }
}

impl From<BitemporalPoint> for i64 {
    fn from(p: BitemporalPoint) -> Self {
        p.0
    }
}

/// Half-open interval `[from, to)` representing a temporal valid or transaction window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BitemporalInterval {
    pub from: i64,
    pub to: i64,
}

impl BitemporalInterval {
    pub fn new(from: i64, to: i64) -> Result<Self> {
        if from > to {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                format!("invalid interval: from ({from}) must be <= to ({to})"),
                false,
            ));
        }
        Ok(Self { from, to })
    }

    /// Standard half-open interval `[from, to)` parsed from strings.
    pub fn between(from: impl AsRef<str>, to: impl AsRef<str>) -> Result<Self> {
        let from_ms = parse_iso_to_ms(from.as_ref())?;
        let to_ms = parse_iso_to_ms(to.as_ref())?;
        Self::new(from_ms, to_ms)
    }

    /// Ergonomic constructor for calendar date ranges where `to_day` is inclusive of that entire day.
    /// E.g. `inclusive_days("2027-01-01", "2027-12-31")` extends from 2027-01-01T00:00:00Z
    /// to 2028-01-01T00:00:00Z (half-open).
    pub fn inclusive_days(from_day: impl AsRef<str>, to_day: impl AsRef<str>) -> Result<Self> {
        let from_ms = parse_iso_to_ms(from_day.as_ref())?;
        let to_str = to_day.as_ref().trim();
        let to_ms = if to_str.eq_ignore_ascii_case("infinity")
            || to_str.eq_ignore_ascii_case("+infinity")
        {
            BITEMPORAL_INFINITY
        } else {
            let base_to = parse_iso_to_ms(to_str)?;
            // If user supplied a date-only YYYY-MM-DD, add 24 hours to include the entire day
            if !to_str.contains('T')
                && !to_str.contains(' ')
                && base_to < BITEMPORAL_INFINITY - 86_400_000
            {
                base_to + 86_400_000
            } else {
                base_to
            }
        };
        Self::new(from_ms, to_ms)
    }

    #[must_use]
    pub fn contains(&self, point: i64) -> bool {
        self.from <= point && point < self.to
    }

    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        self.from < other.to && self.to > other.from
    }

    #[must_use]
    pub fn is_adjacent(&self, other: &Self) -> bool {
        self.to == other.from || other.to == self.from
    }

    #[must_use]
    pub fn intersection(&self, other: &Self) -> Option<Self> {
        let start = self.from.max(other.from);
        let end = self.to.min(other.to);
        if start < end {
            Some(Self {
                from: start,
                to: end,
            })
        } else {
            None
        }
    }

    /// Splits `self` by `new_slice`.
    /// Returns `(Option<prefix>, Option<overlap>, Option<suffix>)`:
    /// - `prefix`: The sub-interval before `new_slice.from` (if any).
    /// - `overlap`: The intersection sub-interval with `new_slice` (if any).
    /// - `suffix`: The sub-interval after `new_slice.to` (if any).
    #[must_use]
    pub fn split_by(&self, new_slice: &Self) -> (Option<Self>, Option<Self>, Option<Self>) {
        let prefix = if self.from < new_slice.from {
            Some(Self {
                from: self.from,
                to: self.to.min(new_slice.from),
            })
        } else {
            None
        };

        let overlap = self.intersection(new_slice);

        let suffix = if self.to > new_slice.to {
            Some(Self {
                from: self.from.max(new_slice.to),
                to: self.to,
            })
        } else {
            None
        };

        (prefix, overlap, suffix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_date_parsing_and_formatting() {
        let ms = parse_iso_to_ms("2026-11-20").unwrap();
        assert_eq!(format_ms_to_date(ms), "2026-11-20");
        assert_eq!(format_ms_to_iso(ms), "2026-11-20T00:00:00.000Z");

        let ms_dt = parse_iso_to_ms("2027-02-01T14:30:15.500Z").unwrap();
        assert_eq!(format_ms_to_iso(ms_dt), "2027-02-01T14:30:15.500Z");

        let ms_inf = parse_iso_to_ms("infinity").unwrap();
        assert_eq!(ms_inf, BITEMPORAL_INFINITY);
        assert_eq!(format_ms_to_iso(ms_inf), "infinity");
    }

    #[test]
    fn test_interval_split() {
        // [2027-01-01, 2027-12-31) split by middle slice [2027-04-01, 2027-07-01)
        let whole = BitemporalInterval::between("2027-01-01", "2027-12-31").unwrap();
        let middle = BitemporalInterval::between("2027-04-01", "2027-07-01").unwrap();

        let (prefix, overlap, suffix) = whole.split_by(&middle);
        assert!(prefix.is_some());
        assert!(overlap.is_some());
        assert!(suffix.is_some());

        let p = prefix.unwrap();
        assert_eq!(format_ms_to_date(p.from), "2027-01-01");
        assert_eq!(format_ms_to_date(p.to), "2027-04-01");

        let o = overlap.unwrap();
        assert_eq!(format_ms_to_date(o.from), "2027-04-01");
        assert_eq!(format_ms_to_date(o.to), "2027-07-01");

        let s = suffix.unwrap();
        assert_eq!(format_ms_to_date(s.from), "2027-07-01");
        assert_eq!(format_ms_to_date(s.to), "2027-12-31");
    }

    #[test]
    fn test_interval_inclusive_days() {
        let interval = BitemporalInterval::inclusive_days("2027-01-01", "2027-12-31").unwrap();
        assert_eq!(format_ms_to_date(interval.from), "2027-01-01");
        assert_eq!(format_ms_to_date(interval.to), "2028-01-01");

        // February 1, 2027 is inside
        let feb1 = parse_iso_to_ms("2027-02-01").unwrap();
        assert!(interval.contains(feb1));

        // December 31, 2027 is inside
        let dec31 = parse_iso_to_ms("2027-12-31").unwrap();
        assert!(interval.contains(dec31));

        // January 1, 2028 is outside (boundary)
        let jan1_2028 = parse_iso_to_ms("2028-01-01").unwrap();
        assert!(!interval.contains(jan1_2028));
    }
}
