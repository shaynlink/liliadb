use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::calendar::{CivilDate, MS_PER_DAY};
use crate::{ErrorCode, LiliaError, Result};

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
        let CivilDate {
            year: y,
            month: m,
            day: d,
        } = CivilDate::from_days(timestamp_ms.div_euclid(MS_PER_DAY));
        match self {
            Self::Monthly => format!("{y:04}-{m:02}"),
            Self::Daily => format!("{y:04}-{m:02}-{d:02}"),
            Self::Yearly => format!("{y:04}"),
        }
    }

    /// Parses a period identifier (e.g. `2026-10`) into its half-open timestamp range `[start_ms, end_ms)`.
    ///
    /// # Errors
    /// `InvalidInput` when `period` is not a valid identifier for this granularity.
    pub fn parse_period_range(self, period: &str) -> Result<(i64, i64)> {
        let period = period.trim();
        let parts: Vec<&str> = period.split('-').collect();
        let (expected_parts, layout) = match self {
            Self::Monthly => (2, "YYYY-MM"),
            Self::Daily => (3, "YYYY-MM-DD"),
            Self::Yearly => (1, "YYYY"),
        };
        if parts.len() != expected_parts {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                format!("invalid {self} shard id '{period}', expected {layout}"),
                false,
            ));
        }
        let year: i64 = parse_component(parts[0], "year")?;
        let month: u32 = parts
            .get(1)
            .map_or(Ok(1), |part| parse_component(part, "month"))?;
        let day: u32 = parts
            .get(2)
            .map_or(Ok(1), |part| parse_component(part, "day"))?;
        let start = CivilDate::new(year, month, day).ok_or_else(|| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("shard id '{period}' is not a calendar date"),
                false,
            )
        })?;
        let end = match self {
            Self::Monthly if month == 12 => CivilDate {
                year: year + 1,
                month: 1,
                day: 1,
            },
            Self::Monthly => CivilDate {
                year,
                month: month + 1,
                day: 1,
            },
            Self::Daily => CivilDate::from_days(start.to_days() + 1),
            Self::Yearly => CivilDate {
                year: year + 1,
                month: 1,
                day: 1,
            },
        };
        Ok((start.to_days() * MS_PER_DAY, end.to_days() * MS_PER_DAY))
    }
}

fn parse_component<T: std::str::FromStr>(part: &str, name: &str) -> Result<T> {
    part.parse().map_err(|_| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            format!("invalid {name} in shard id"),
            false,
        )
    })
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

    fn days_of(year: i64, month: u32, day: u32) -> i64 {
        CivilDate::new(year, month, day).unwrap().to_days()
    }

    #[test]
    fn test_monthly_granularity() {
        let gran = ShardGranularity::Monthly;
        // 2026-10-15 12:00:00 UTC
        let days = days_of(2026, 10, 15);
        let ts = days * 86_400_000 + 43_200_000;
        assert_eq!(gran.format_period(ts), "2026-10");

        let (start, end) = gran.parse_period_range("2026-10").unwrap();
        assert_eq!(start, days_of(2026, 10, 1) * 86_400_000);
        assert_eq!(end, days_of(2026, 11, 1) * 86_400_000);
        // Early-month days used to be attributed to the following year.
        assert_eq!(
            gran.format_period(days_of(1970, 1, 10) * 86_400_000),
            "1970-01"
        );
        assert_eq!(
            gran.format_period(days_of(2026, 12, 31) * 86_400_000),
            "2026-12"
        );
        assert!(gran.parse_period_range("2026-13").is_err());
    }

    #[test]
    fn test_daily_granularity() {
        let gran = ShardGranularity::Daily;
        let days = days_of(2026, 10, 15);
        let ts = days * 86_400_000;
        assert_eq!(gran.format_period(ts), "2026-10-15");
        assert!(gran.parse_period_range("2026-02-30").is_err());

        let (start, end) = gran.parse_period_range("2026-10-15").unwrap();
        assert_eq!(start, days * 86_400_000);
        assert_eq!(end, (days + 1) * 86_400_000);
    }
}
