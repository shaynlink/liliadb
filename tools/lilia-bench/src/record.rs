//! One measurement, as exchanged between the suite and its engine processes (one JSON line each).

use serde::{Deserialize, Serialize};

/// How a commit reaches stable storage, as declared by the engine adapter for the current OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncPrimitive {
    /// `fcntl(F_FULLFSYNC)`: flushes the drive cache (Apple platforms).
    FullFsync,
    /// `fsync`/`fdatasync`: durable on Linux, stops at the drive cache on Apple platforms.
    Fsync,
    /// `fcntl(F_BARRIERFSYNC)`: ordered writes, not power-safe (Apple platforms).
    Barrier,
    /// `FlushFileBuffers` (Windows).
    FlushFileBuffers,
    /// No sync on commit: not a durable configuration.
    None,
}

impl SyncPrimitive {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::FullFsync => "F_FULLFSYNC",
            Self::Fsync => "fsync",
            Self::Barrier => "F_BARRIERFSYNC",
            Self::FlushFileBuffers => "FlushFileBuffers",
            Self::None => "none",
        }
    }

    /// Whether an acknowledged commit survives power loss on `os`.
    #[must_use]
    pub fn power_safe_on(self, os: &str) -> bool {
        match self {
            Self::FullFsync | Self::FlushFileBuffers => true,
            Self::Fsync => os != "macos" && os != "ios",
            Self::Barrier | Self::None => false,
        }
    }
}

/// Engine identity carried by every record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineInfo {
    /// Stable identifier used on the command line, e.g. `sqlite-fullfsync`.
    pub id: String,
    /// Human name, e.g. `SQLite (WAL, fullfsync)`.
    pub name: String,
    pub version: String,
    pub sync: SyncPrimitive,
}

/// Latency distribution of the timed unit (one operation, or one batch for batch workloads).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Latency {
    pub samples: u64,
    pub mean_us: f64,
    pub p50_us: f64,
    pub p95_us: f64,
    pub p99_us: f64,
    pub p999_us: f64,
    pub max_us: f64,
}

impl Latency {
    /// Summarises nanosecond samples; sorts them in place.
    #[must_use]
    pub fn from_nanos(samples: &mut [u64]) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        samples.sort_unstable();
        let at = |q: f64| -> f64 {
            let rank = (q * (samples.len() - 1) as f64).round() as usize;
            samples[rank.min(samples.len() - 1)] as f64 / 1e3
        };
        let total: u128 = samples.iter().map(|&n| u128::from(n)).sum();
        Self {
            samples: samples.len() as u64,
            mean_us: total as f64 / samples.len() as f64 / 1e3,
            p50_us: at(0.50),
            p95_us: at(0.95),
            p99_us: at(0.99),
            p999_us: at(0.999),
            max_us: samples[samples.len() - 1] as f64 / 1e3,
        }
    }
}

/// Process resource counters consumed by one workload (deltas, not totals).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Io {
    /// Bytes the process sent to the storage device.
    pub device_bytes_written: u64,
    /// Bytes the process read from the storage device (page-cache hits excluded).
    pub device_bytes_read: u64,
}

/// One measured workload on one engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    /// `engine` (Rust suite) or `sdk` (JavaScript surfaces).
    pub suite: String,
    pub engine: EngineInfo,
    /// Dotted workload name, e.g. `write.concurrent`.
    pub workload: String,
    /// Workload parameters that distinguish rows of the same workload (threads, batch, size…).
    pub params: serde_json::Map<String, serde_json::Value>,
    /// Logical operations completed (keys written, keys read, queries answered, rows scanned).
    pub ops: u64,
    pub seconds: f64,
    pub ops_per_sec: f64,
    pub latency: Latency,
    /// Payload bytes the workload asked the engine to store or return.
    pub logical_bytes: u64,
    pub io: Io,
    /// Size of the database directory after the workload, engine closed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_bytes: Option<u64>,
    /// Peak resident set size of the engine process.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_rss_bytes: Option<u64>,
    /// Workload-specific quality metric (vector recall@k, rows matched…).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<serde_json::Value>,
}

/// A planned run that produced no measurement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Failure {
    pub suite: String,
    pub engine: String,
    pub workload: String,
    pub reason: String,
}

/// Everything a report is rendered from: written as JSON lines, one object per line.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Line {
    Host(Box<crate::sysinfo::Host>),
    Record(Box<Record>),
    Failure(Failure),
}

/// Builds a `params` map from `(name, value)` pairs.
#[must_use]
pub fn params<const N: usize>(pairs: [(&str, serde_json::Value); N]) -> serde_json::Map<String, serde_json::Value> {
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}
