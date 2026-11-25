use crate::error::{ErrorCode, LiliaError, Result};
use serde::{Deserialize, Serialize};

pub const BITEMPORAL_INFINITY: i64 = i64::MAX;
pub const BITEMPORAL_NEG_INFINITY: i64 = i64::MIN;

/// Howard Hinnant's algorithm: Gregorian calendar calculation (days since 1970-01-01).
#[allow(
    clippy::cast_sign_loss,
    reason = "the year of era is in 0..400 by construction"
)]
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
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the day of era is in 0..146097 by construction, and the year of an i64 millisecond instant is within ±300 million"
)]
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
///
/// # Errors
///
/// [`ErrorCode::InvalidInput`] for a string in none of these forms, or a date component out of
/// range.
pub fn parse_iso_to_ms(s: &str) -> Result<i64> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            "cannot parse empty timestamp",
            false,
        ));
    }
    if let Some(ms) = keyword_ms(&trimmed.to_ascii_lowercase()) {
        return Ok(ms);
    }
    if let Ok(num) = trimmed.parse::<i64>() {
        return Ok(num);
    }
    let (date_part, time_part) = match trimmed.split_once('T').or_else(|| trimmed.split_once(' ')) {
        Some((d, t)) => (d, Some(t)),
        None => (trimmed, None),
    };
    let midnight = date_ms(date_part, trimmed)?;
    match time_part {
        Some(time) => Ok(midnight + daytime_ms(time)?),
        None => Ok(midnight),
    }
}

/// The instant a keyword (`infinity`, `min`, `now`, ...) names, if `lowered` is one.
fn keyword_ms(lowered: &str) -> Option<i64> {
    match lowered {
        "infinity" | "+infinity" | "max" | "forever" => Some(BITEMPORAL_INFINITY),
        "-infinity" | "min" => Some(BITEMPORAL_NEG_INFINITY),
        "now" | "current" => {
            #[cfg(not(target_arch = "wasm32"))]
            {
                Some(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX)),
                )
            }
            #[cfg(target_arch = "wasm32")]
            {
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "the clock is a whole number of milliseconds, far below 2^53"
                )]
                Some(js_sys::Date::now() as i64)
            }
        }
        _ => None,
    }
}

/// `text` parsed as a number, or an error naming it as `what`.
fn number<T: std::str::FromStr>(text: &str, what: &str) -> Result<T> {
    text.parse().map_err(|_| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid {what}: {text}"),
            false,
        )
    })
}

/// Milliseconds at midnight UTC of `YYYY-MM-DD`; `whole` is the full input, for errors.
fn date_ms(date_part: &str, whole: &str) -> Result<i64> {
    let segments: Vec<&str> = date_part.split('-').collect();
    let [year, month, day] = segments.as_slice() else {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid date format (expected YYYY-MM-DD): {whole}"),
            false,
        ));
    };
    let year: i32 = number(year, "year")?;
    let month: u32 = number(month, "month")?;
    let day: u32 = number(day, "day")?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            format!("date component out of range: {year}-{month:02}-{day:02}"),
            false,
        ));
    }
    Ok(days_from_civil(year, month, day) * 86_400_000)
}

/// Milliseconds since midnight UTC of `HH:MM[:SS[.fff]][Z|±HH[:MM]]`.
fn daytime_ms(time: &str) -> Result<i64> {
    let (time, offset_ms) = if let Some(local) = time.strip_suffix(['Z', 'z']) {
        (local, 0)
    } else if let Some((local, offset)) = time.rsplit_once('+') {
        (local, parse_tz_offset(offset)?)
    } else if let Some((local, offset)) = time.rsplit_once('-') {
        (local, -parse_tz_offset(offset)?)
    } else {
        (time, 0)
    };
    let segments: Vec<&str> = time.split(':').collect();
    let (hour, minute, second) = match segments.as_slice() {
        [hour, minute] => (*hour, *minute, None),
        [hour, minute, second, ..] => (*hour, *minute, Some(*second)),
        _ => {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                format!("invalid time format: {time}"),
                false,
            ))
        }
    };
    let hour: i64 = number(hour, "hour")?;
    let minute: i64 = number(minute, "minute")?;
    let (sec, millis) = second.map_or((0, 0), seconds);
    Ok(hour * 3_600_000 + minute * 60_000 + sec * 1_000 + millis - offset_ms)
}

/// Seconds and milliseconds of `SS[.fff...]`; digits beyond milliseconds are dropped.
fn seconds(text: &str) -> (i64, i64) {
    let Some((whole, fraction)) = text.split_once('.') else {
        return (text.parse().unwrap_or(0), 0);
    };
    let millis: String = fraction
        .chars()
        .chain(std::iter::repeat('0'))
        .take(3)
        .collect();
    (whole.parse().unwrap_or(0), millis.parse().unwrap_or(0))
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

    /// The instant `val` names, in any form of [`parse_iso_to_ms`].
    ///
    /// # Errors
    ///
    /// As [`parse_iso_to_ms`].
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
    /// The half-open interval `[from, to)`, in epoch milliseconds.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidInput`] when `from` is after `to`.
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
    ///
    /// # Errors
    ///
    /// As [`parse_iso_to_ms`] and [`BitemporalInterval::new`].
    pub fn between(from: impl AsRef<str>, to: impl AsRef<str>) -> Result<Self> {
        let from_ms = parse_iso_to_ms(from.as_ref())?;
        let to_ms = parse_iso_to_ms(to.as_ref())?;
        Self::new(from_ms, to_ms)
    }

    /// Ergonomic constructor for calendar date ranges where `to_day` is inclusive of that entire day.
    /// E.g. `inclusive_days("2027-01-01", "2027-12-31")` extends from 2027-01-01T00:00:00Z
    /// to 2028-01-01T00:00:00Z (half-open).
    ///
    /// # Errors
    ///
    /// As [`parse_iso_to_ms`] and [`BitemporalInterval::new`].
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

        let offset = parse_iso_to_ms("2027-02-01T16:30:15.5+02:00").unwrap();
        assert_eq!(offset, ms_dt);
        let fractions = parse_iso_to_ms("2027-02-01T14:30:15.500999Z").unwrap();
        assert_eq!(fractions, ms_dt);
        let nanoseconds = parse_iso_to_ms("2027-02-01T14:30:15.5009998887776665554443Z").unwrap();
        assert_eq!(nanoseconds, ms_dt);
        let empty_fraction = parse_iso_to_ms("2027-02-01T14:30:15.").unwrap();
        assert_eq!(empty_fraction, ms_dt - 500);
        assert!(parse_iso_to_ms("2027-02-01T14").is_err());
        assert!(parse_iso_to_ms("2027-13-01").is_err());
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
