use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{ErrorCode, LiliaError, Result};

/// Date helpers using the Howard Hinnant proleptic Gregorian algorithm.
pub fn days_to_ymd(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1024 + doe / 1461 - doe / 142_401) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

pub fn ymd_to_days(year: i32, month: u32, day: u32) -> i64 {
    let y = if month <= 2 {
        (year as i64) - 1
    } else {
        year as i64
    };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = (y - era * 400) as u32;
    let m = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + (doe as i64) - 719_468
}

/// Partitioning granularity defining the temporal boundaries of individual shard databases.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShardGranularity {
    /// Monthly sharding (e.g. `2026-10`, `2026-11`). Standard for mobile apps and telemetry.
    #[default]
    Monthly,
    /// Daily sharding (e.g. `2026-10-24`). For high-throughput daily partitions.
    Daily,
    /// Yearly sharding (e.g. `2026`). For low-volume long-term archival.
    Yearly,
}

impl ShardGranularity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Monthly => "monthly",
            Self::Daily => "daily",
            Self::Yearly => "yearly",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "monthly" => Some(Self::Monthly),
            "daily" => Some(Self::Daily),
            "yearly" => Some(Self::Yearly),
            _ => None,
        }
    }

    /// Formats a UNIX timestamp (milliseconds) into the canonical shard identifier string.
    #[must_use]
    pub fn format_period(self, timestamp_ms: i64) -> String {
        let days = timestamp_ms.div_euclid(86_400_000);
        let (y, m, d) = days_to_ymd(days);
        match self {
            Self::Monthly => format!("{y:04}-{m:02}"),
            Self::Daily => format!("{y:04}-{m:02}-{d:02}"),
            Self::Yearly => format!("{y:04}"),
        }
    }

    /// Parses a period identifier (e.g. `2026-10`) into its half-open timestamp range `[start_ms, end_ms)`.
    pub fn parse_period_range(self, period: &str) -> Result<(i64, i64)> {
        let period = period.trim();
        match self {
            Self::Monthly => {
                let parts: Vec<&str> = period.split('-').collect();
                if parts.len() != 2 {
                    return Err(LiliaError::new(
                        ErrorCode::InvalidInput,
                        format!("invalid monthly shard id '{period}', expected YYYY-MM"),
                        false,
                    ));
                }
                let year: i32 = parts[0].parse().map_err(|_| {
                    LiliaError::new(ErrorCode::InvalidInput, "invalid year in shard id", false)
                })?;
                let month: u32 = parts[1].parse().map_err(|_| {
                    LiliaError::new(ErrorCode::InvalidInput, "invalid month in shard id", false)
                })?;
                if !(1..=12).contains(&month) {
                    return Err(LiliaError::new(
                        ErrorCode::InvalidInput,
                        format!("month must be 1..=12, got {month}"),
                        false,
                    ));
                }
                let start_days = ymd_to_days(year, month, 1);
                let (next_y, next_m) = if month == 12 {
                    (year + 1, 1)
                } else {
                    (year, month + 1)
                };
                let end_days = ymd_to_days(next_y, next_m, 1);
                Ok((start_days * 86_400_000, end_days * 86_400_000))
            }
            Self::Daily => {
                let parts: Vec<&str> = period.split('-').collect();
                if parts.len() != 3 {
                    return Err(LiliaError::new(
                        ErrorCode::InvalidInput,
                        format!("invalid daily shard id '{period}', expected YYYY-MM-DD"),
                        false,
                    ));
                }
                let year: i32 = parts[0].parse().map_err(|_| {
                    LiliaError::new(ErrorCode::InvalidInput, "invalid year in shard id", false)
                })?;
                let month: u32 = parts[1].parse().map_err(|_| {
                    LiliaError::new(ErrorCode::InvalidInput, "invalid month in shard id", false)
                })?;
                let day: u32 = parts[2].parse().map_err(|_| {
                    LiliaError::new(ErrorCode::InvalidInput, "invalid day in shard id", false)
                })?;
                let start_days = ymd_to_days(year, month, day);
                let end_days = start_days + 1;
                Ok((start_days * 86_400_000, end_days * 86_400_000))
            }
            Self::Yearly => {
                let year: i32 = period.parse().map_err(|_| {
                    LiliaError::new(ErrorCode::InvalidInput, "invalid year in shard id", false)
                })?;
                let start_days = ymd_to_days(year, 1, 1);
                let end_days = ymd_to_days(year + 1, 1, 1);
                Ok((start_days * 86_400_000, end_days * 86_400_000))
            }
        }
    }
}

impl fmt::Display for ShardGranularity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Operational state of a local shard database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShardStatus {
    /// Active hot shard receiving current writes and serving reads.
    Active,
    /// Warm past shard attached via SQLite `ATTACH DATABASE` (read-only or backfill).
    Warm,
    /// Cold offline archive compressed with Zstandard (`.zst`), detached from active connections.
    Cold,
    /// Uncompressed database file that is currently detached from active SQLite connections.
    Detached,
}

impl ShardStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Warm => "warm",
            Self::Cold => "cold",
            Self::Detached => "detached",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "active" => Some(Self::Active),
            "warm" => Some(Self::Warm),
            "cold" => Some(Self::Cold),
            "detached" => Some(Self::Detached),
            _ => None,
        }
    }

    #[must_use]
    pub const fn is_attached(self) -> bool {
        matches!(self, Self::Active | Self::Warm)
    }
}

impl fmt::Display for ShardStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Configuration options for the Local Sharding & Dynamic Archiving Engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardingConfig {
    /// Temporal partition boundary granularity.
    pub granularity: ShardGranularity,
    /// Automatically create and rollover to the next temporal partition when time crosses boundary.
    pub auto_rollover: bool,
    /// Maximum number of warm attached shards to maintain in the SQLite connection pool (default: 12).
    pub max_attached_shards: usize,
    /// Directory where partition files and archives are stored. If None, defaults to `<db_path>.shards/`.
    pub shards_dir: Option<PathBuf>,
    /// Zstandard compression level for cold tier archives (default: 3 for balanced mobile performance).
    pub zstd_level: i32,
    /// When true, automatically detaches and compresses the oldest warm shards once `max_attached_shards` is exceeded.
    pub auto_archive_oldest: bool,
}

impl Default for ShardingConfig {
    fn default() -> Self {
        Self {
            granularity: ShardGranularity::Monthly,
            auto_rollover: true,
            max_attached_shards: 12,
            shards_dir: None,
            zstd_level: 3,
            auto_archive_oldest: false,
        }
    }
}

impl ShardingConfig {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub const fn with_granularity(mut self, granularity: ShardGranularity) -> Self {
        self.granularity = granularity;
        self
    }

    #[must_use]
    pub const fn with_auto_rollover(mut self, auto_rollover: bool) -> Self {
        self.auto_rollover = auto_rollover;
        self
    }

    #[must_use]
    pub const fn with_max_attached_shards(mut self, max: usize) -> Self {
        self.max_attached_shards = max;
        self
    }

    #[must_use]
    pub fn with_shards_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.shards_dir = Some(dir.into());
        self
    }

    #[must_use]
    pub const fn with_zstd_level(mut self, level: i32) -> Self {
        self.zstd_level = level;
        self
    }

    #[must_use]
    pub const fn with_auto_archive_oldest(mut self, auto: bool) -> Self {
        self.auto_archive_oldest = auto;
        self
    }
}

/// Metadata describing a single temporal shard partition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardInfo {
    /// Canonical shard identifier (e.g. `2026-10`).
    pub shard_id: String,
    /// Operational status (`active`, `warm`, `cold`, `detached`).
    pub status: ShardStatus,
    /// Start timestamp (inclusive, in milliseconds since UNIX epoch).
    pub period_start_ms: i64,
    /// End timestamp (exclusive, in milliseconds since UNIX epoch).
    pub period_end_ms: i64,
    /// Local filesystem path to the database or archive file.
    pub file_path: PathBuf,
    /// Whether the file is currently stored in compressed Zstandard format.
    pub is_compressed: bool,
    /// Uncompressed size on disk in bytes (or uncompressed size before archiving).
    pub uncompressed_bytes: u64,
    /// Compressed size in bytes if compressed, otherwise 0.
    pub compressed_bytes: u64,
    /// Cryptographic checksum (Blake3 hex string).
    pub checksum: Option<String>,
    /// Creation timestamp (milliseconds since UNIX epoch).
    pub created_at_ms: i64,
    /// Timestamp when this shard was archived into cold tier, if applicable.
    pub archived_at_ms: Option<i64>,
}

/// Result returned after successfully executing `detach_and_compress`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardArchiveResult {
    /// Canonical shard identifier (e.g. `2026-08`).
    pub shard_id: String,
    /// Path to the newly created `.zst` archive file.
    pub archive_path: PathBuf,
    /// Size of the uncompressed SQLite database before compression.
    pub original_size_bytes: u64,
    /// Size of the resulting Zstandard archive file.
    pub compressed_size_bytes: u64,
    /// Compression ratio: `compressed_size_bytes as f64 / original_size_bytes as f64`.
    pub compression_ratio: f64,
    /// Cryptographic Blake3 checksum of the uncompressed content.
    pub checksum: String,
    /// Milliseconds elapsed during the archive process.
    pub duration_ms: u64,
    /// Timestamp when archive was created.
    pub archived_at_ms: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ymd_conversion_roundtrip() {
        let (y, m, d) = (2026, 11, 13);
        let days = ymd_to_days(y, m, d);
        let (y2, m2, d2) = days_to_ymd(days);
        assert_eq!((y, m, d), (y2, m2, d2));
    }

    #[test]
    fn test_monthly_granularity() {
        let gran = ShardGranularity::Monthly;
        // 2026-10-15 12:00:00 UTC
        let days = ymd_to_days(2026, 10, 15);
        let ts = days * 86_400_000 + 43_200_000;
        assert_eq!(gran.format_period(ts), "2026-10");

        let (start, end) = gran.parse_period_range("2026-10").unwrap();
        assert_eq!(start, ymd_to_days(2026, 10, 1) * 86_400_000);
        assert_eq!(end, ymd_to_days(2026, 11, 1) * 86_400_000);
    }

    #[test]
    fn test_daily_granularity() {
        let gran = ShardGranularity::Daily;
        let days = ymd_to_days(2026, 10, 15);
        let ts = days * 86_400_000;
        assert_eq!(gran.format_period(ts), "2026-10-15");

        let (start, end) = gran.parse_period_range("2026-10-15").unwrap();
        assert_eq!(start, days * 86_400_000);
        assert_eq!(end, (days + 1) * 86_400_000);
    }
}
