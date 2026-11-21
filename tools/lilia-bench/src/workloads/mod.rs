//! Workload groups. Each group runs in its own process (`lilia-bench run`) and prints one
//! [`Line`] per measurement on standard output.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::engines::{self, Engine, Result};
use crate::os;
use crate::record::{EngineInfo, Io, Latency, Line, Record};

pub mod analytics;
pub mod fts;
pub mod json;
pub mod read;
pub mod vector;
pub mod write;
pub mod ycsb;

/// Sizes and time limits shared by every group of one suite run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Records loaded before read and mixed workloads.
    pub records: u64,
    /// Value size of the key-value workloads, in bytes.
    pub value_size: usize,
    /// Wall-clock budget of each time-boxed workload.
    pub budget_ms: u64,
    /// Writer thread counts of the concurrent commit workloads.
    pub threads: Vec<usize>,
    /// Reader thread counts of the concurrent point-read workloads.
    pub read_threads: Vec<usize>,
    /// Documents loaded by the JSON and full-text groups.
    pub documents: u64,
    /// Vectors indexed by the vector group, and their dimension.
    pub vectors: u64,
    pub vector_dim: usize,
    /// Rows of the analytics group.
    pub analytics_rows: u64,
    /// Directory holding the databases (one fresh subdirectory per workload).
    pub work_dir: PathBuf,
}

impl Settings {
    #[must_use]
    pub fn budget(&self) -> Duration {
        Duration::from_millis(self.budget_ms)
    }
}

/// The groups, in the order the suite runs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Group {
    Write,
    Read,
    Ycsb,
    Json,
    Fts,
    Vector,
    Analytics,
}

impl Group {
    pub const ALL: [Self; 7] = [Self::Write, Self::Read, Self::Ycsb, Self::Json, Self::Fts, Self::Vector, Self::Analytics];

    /// Engines compiled into this binary that implement this group.
    #[must_use]
    pub fn engines(self) -> Vec<&'static str> {
        let models: &[&str] = match self {
            Self::Write | Self::Read | Self::Ycsb => return engines::available(),
            Self::Json | Self::Fts => &["lilia", "sqlite"],
            Self::Vector => &["lilia"],
            Self::Analytics => &["lilia", "sqlite", "duckdb"],
        };
        let compiled = [
            ("lilia", cfg!(feature = "lilia")),
            ("sqlite", cfg!(feature = "sqlite")),
            ("duckdb", cfg!(feature = "duckdb")),
        ];
        models
            .iter()
            .copied()
            .filter(|m| compiled.iter().any(|(name, on)| name == m && *on))
            .collect()
    }
}

/// Runs `group` against `engine`, emitting each measurement through `emit`.
pub fn run(group: Group, engine: &str, settings: &Settings, emit: &mut dyn FnMut(Line)) -> Result<()> {
    match group {
        Group::Write => write::run(engine, settings, emit),
        Group::Read => read::run(engine, settings, emit),
        Group::Ycsb => ycsb::run(engine, settings, emit),
        Group::Json => json::run(engine, settings, emit),
        Group::Fts => fts::run(engine, settings, emit),
        Group::Vector => vector::run(engine, settings, emit),
        Group::Analytics => analytics::run(engine, settings, emit),
    }
}

/// A fresh, empty database directory under the work directory; removed on drop.
pub fn fresh_dir(settings: &Settings) -> Result<tempfile::TempDir> {
    std::fs::create_dir_all(&settings.work_dir)?;
    Ok(tempfile::Builder::new().prefix("lilia-bench-").tempdir_in(&settings.work_dir)?)
}

/// Opens `engine` in `dir`.
pub fn open(engine: &str, dir: &Path) -> Result<Box<dyn Engine>> {
    engines::open(engine, dir)
}

/// Clock and I/O counters at the start of a measured section.
#[derive(Debug, Clone, Copy)]
pub struct Meter {
    started: Instant,
    io: Io,
}

impl Meter {
    #[must_use]
    pub fn start() -> Self {
        Self { started: Instant::now(), io: os::io_totals() }
    }

    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Closes the section into a record. `latencies` are nanoseconds of the timed unit.
    #[must_use]
    pub fn finish(
        self,
        engine: EngineInfo,
        workload: &str,
        params: serde_json::Map<String, serde_json::Value>,
        ops: u64,
        logical_bytes: u64,
        latencies: &mut [u64],
    ) -> Record {
        let seconds = self.started.elapsed().as_secs_f64();
        Record {
            suite: "engine".into(),
            engine,
            workload: workload.into(),
            params,
            ops,
            seconds,
            ops_per_sec: if seconds > 0.0 { ops as f64 / seconds } else { 0.0 },
            latency: Latency::from_nanos(latencies),
            logical_bytes,
            io: os::io_delta(self.io, os::io_totals()),
            disk_bytes: None,
            peak_rss_bytes: os::peak_rss_bytes(),
            quality: None,
        }
    }
}

/// Nanoseconds since `start`, saturated.
#[must_use]
pub fn nanos_since(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Loads records `0..count` (batches of 1,000) without measuring; used to prepare read workloads.
pub fn preload(engine: &dyn Engine, count: u64, value_size: usize) -> Result<()> {
    let mut session = engine.session()?;
    let mut index = 0;
    while index < count {
        let end = (index + 1_000).min(count);
        let rows: Vec<([u8; 16], Vec<u8>)> = (index..end).map(|i| (crate::keys::key(i), crate::keys::value(i, value_size))).collect();
        let items: Vec<(&[u8], &[u8])> = rows.iter().map(|(k, v)| (&k[..], &v[..])).collect();
        session.put(&items)?;
        index = end;
    }
    Ok(())
}

/// Fails the run when an engine returned fewer results than the data guarantees: a fast wrong
/// answer must never be reported as a measurement.
pub fn expect_found(engine: &str, workload: &str, found: u64, expected: u64) -> Result<()> {
    if found == expected {
        Ok(())
    } else {
        Err(format!("{engine} {workload}: found {found} of {expected} keys that exist").into())
    }
}
