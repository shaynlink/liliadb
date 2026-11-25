//! The state of the storage: its shape, and checks that the stored data reads back intact.

use std::ops::Bound;

use lilia_core::{LedgerVerificationReport, LedgerVerifyMode};
use serde::{Deserialize, Serialize};

use super::Database;
use crate::error::Result;

/// The sorted runs of one level.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[rkyv(crate = rkyv, derive(Debug))]
pub struct LevelStats {
    pub runs: usize,
    pub bytes: u64,
}

/// The shape of the storage.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[rkyv(crate = rkyv, derive(Debug))]
pub struct StorageStats {
    /// Level 0 first.
    pub levels: Vec<LevelStats>,
    /// Bytes in the memtable that takes writes.
    pub memtable_bytes: usize,
    /// Full memtables waiting for their flush.
    pub immutable_memtables: usize,
    /// The newest version written.
    pub version: u64,
    /// The oldest version still readable ([`Database::oldest_version`]).
    pub oldest_version: u64,
}

/// What [`Database::check_integrity`] found.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IntegrityReport {
    /// Whether every entry read back and the ledger, if any, verified.
    pub ok: bool,
    /// Entries read before the end or the first failure.
    pub entries: u64,
    /// The first failure, if any.
    pub error: Option<String>,
    /// The verification of the ledger, when the database keeps one.
    pub ledger: Option<LedgerVerificationReport>,
}

impl Database {
    /// The shape of the storage.
    #[must_use]
    pub fn storage_stats(&self) -> StorageStats {
        let stats = self.engine.stats();
        StorageStats {
            levels: stats
                .levels
                .iter()
                .map(|&(runs, bytes)| LevelStats { runs, bytes })
                .collect(),
            memtable_bytes: stats.memtable_bytes,
            immutable_memtables: stats.immutable_memtables,
            version: stats.sequence,
            oldest_version: self.engine.oldest_sequence(),
        }
    }

    /// Reads every stored entry of one state of the database, so that every data block is read
    /// and its checksum verified, then verifies the ledger strictly if the database keeps one.
    /// With `quick`, only the ledger is verified, and fast.
    ///
    /// # Errors
    ///
    /// Engine errors other than a corrupted block, which the report holds.
    pub fn check_integrity(&self, quick: bool) -> Result<IntegrityReport> {
        let mut entries = 0u64;
        let mut error = None;
        if !quick {
            let snapshot = self.engine.snapshot();
            for entry in self
                .engine
                .range_at(&snapshot, Bound::Unbounded, Bound::Unbounded)?
            {
                match entry {
                    Ok(_) => entries += 1,
                    Err(failure) => {
                        error = Some(failure.to_string());
                        break;
                    }
                }
            }
        }
        let ledger = match &self.ledger {
            Some(_) => {
                let mode = if quick {
                    LedgerVerifyMode::Fast
                } else {
                    LedgerVerifyMode::Strict
                };
                Some(self.verify_ledger(mode)?)
            }
            None => None,
        };
        let ledger_ok = ledger.as_ref().is_none_or(|report| report.is_valid);
        Ok(IntegrityReport {
            ok: error.is_none() && ledger_ok,
            entries,
            error,
            ledger,
        })
    }
}
