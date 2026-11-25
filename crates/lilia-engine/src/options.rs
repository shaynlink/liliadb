//! Engine configuration, sized from the machine by default.

use lilia_io::{HardwareProfile, SyncLevel};

use crate::error::{Error, Result};

/// Number of levels of sorted runs.
pub const LEVELS: usize = 7;

/// What a successful write guarantees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Durability {
    /// Every acknowledged commit survives power loss: one full flush of the journal per commit
    /// group (`F_FULLFSYNC` on Apple platforms, `fdatasync` on Linux).
    #[default]
    Durable,
    /// The journal stays consistent across power loss, but the most recent acknowledged commits
    /// may be lost: one write barrier per commit group (`F_BARRIERFSYNC`; `fdatasync` elsewhere).
    Balanced,
    /// No sync on commit: acknowledged commits survive a process crash, not power loss.
    Performance,
}

impl Durability {
    /// Sync after each commit group.
    pub(crate) const fn commit_sync(self) -> Option<SyncLevel> {
        match self {
            Self::Durable => Some(SyncLevel::Full),
            Self::Balanced => Some(SyncLevel::Barrier),
            Self::Performance => None,
        }
    }

    /// Sync of new sorted runs and manifest records, which must reach the device before the
    /// manifest references them.
    pub(crate) const fn metadata_sync(self) -> SyncLevel {
        match self {
            Self::Durable => SyncLevel::Full,
            Self::Balanced | Self::Performance => SyncLevel::Barrier,
        }
    }
}

/// Engine configuration. [`Options::for_hardware`] sizes every buffer from the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub durability: Durability,
    /// Memtable size that triggers a switch to a new memtable and a flush of the old one.
    pub memtable_bytes: usize,
    /// Memtables waiting for their flush before writes stall.
    pub max_immutable_memtables: usize,
    /// Target size of a data block of a sorted run.
    pub block_bytes: usize,
    pub bloom_bits_per_key: usize,
    pub block_cache_bytes: usize,
    /// Level-0 runs that trigger a compaction into level 1.
    pub l0_compaction_trigger: usize,
    /// Level-0 runs at which writes stall until compaction catches up.
    pub l0_stop_writes: usize,
    /// Size budget of level 1; each deeper level holds `level_multiplier` times more.
    pub level1_bytes: u64,
    pub level_multiplier: u64,
    /// Target size of a sorted run written by a compaction.
    pub target_file_bytes: u64,
    /// Bytes of batches one commit group may take.
    pub max_group_bytes: usize,
    /// Background compaction threads (flushes have their own thread).
    pub compaction_threads: usize,
    /// Run flushes and compactions on background threads. Without them (the default on
    /// `wasm32`, where threads are not available), the writer that fills a memtable flushes it
    /// and compacts the levels that are due before its write completes.
    pub background_threads: bool,
    pub create_if_missing: bool,
    /// Keep old versions for reads in the past ([`crate::Db::snapshot_at`]): compactions keep
    /// every version visible at or after the history horizon, which starts at the oldest data and
    /// only moves forward through [`crate::Db::set_history_horizon`].
    pub retain_history: bool,
}

impl Options {
    /// Buffers sized from `hardware`: memtables and cache from memory, blocks from the page size,
    /// compaction threads from the efficiency cores (or a quarter of the cores on symmetric
    /// machines), so maintenance does not take the cores the application runs on.
    #[must_use]
    pub fn for_hardware(hardware: &HardwareProfile) -> Self {
        let memory = usize::try_from(hardware.memory_bytes).unwrap_or(usize::MAX);
        let memtable_bytes = (memory / 512).clamp(4 << 20, 64 << 20);
        let background = if hardware.efficiency_cores > 0 {
            hardware.efficiency_cores
        } else {
            hardware.performance_cores / 4
        };
        Self {
            durability: Durability::Durable,
            memtable_bytes,
            max_immutable_memtables: 2,
            block_bytes: hardware.page_size.clamp(4 << 10, 64 << 10),
            bloom_bits_per_key: 10,
            block_cache_bytes: (memory / 32).clamp(8 << 20, 512 << 20),
            l0_compaction_trigger: 4,
            l0_stop_writes: 12,
            level1_bytes: 4 * memtable_bytes as u64,
            level_multiplier: 10,
            target_file_bytes: 2 * memtable_bytes as u64,
            max_group_bytes: 4 << 20,
            compaction_threads: background.clamp(1, 4),
            create_if_missing: true,
            retain_history: false,
            background_threads: !cfg!(target_arch = "wasm32"),
        }
    }

    pub(crate) fn validate(&self) -> Result<()> {
        let fail = |what: &str| Err(Error::InvalidArgument(what.to_owned()));
        if self.memtable_bytes < 4 << 10 {
            return fail("memtable_bytes must be at least 4 KiB");
        }
        if self.block_bytes < 256 || self.target_file_bytes < self.block_bytes as u64 {
            return fail("block_bytes must be at least 256 and at most target_file_bytes");
        }
        if self.max_immutable_memtables == 0
            || self.compaction_threads == 0
            || self.max_group_bytes == 0
        {
            return fail(
                "max_immutable_memtables, compaction_threads and max_group_bytes must be positive",
            );
        }
        if self.l0_compaction_trigger == 0 || self.l0_stop_writes <= self.l0_compaction_trigger {
            return fail("l0_stop_writes must exceed a positive l0_compaction_trigger");
        }
        if self.level1_bytes == 0 || self.level_multiplier < 2 {
            return fail("level1_bytes must be positive and level_multiplier at least 2");
        }
        Ok(())
    }

    /// Size budget of `level` (1 and deeper).
    pub(crate) fn level_bytes(&self, level: usize) -> u64 {
        let exponent = u32::try_from(level.saturating_sub(1)).unwrap_or(u32::MAX);
        self.level1_bytes
            .saturating_mul(self.level_multiplier.saturating_pow(exponent))
    }
}

impl Default for Options {
    fn default() -> Self {
        Self::for_hardware(&HardwareProfile::probe())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardware_sizing_is_valid_on_small_and_large_machines() {
        let small = Options::for_hardware(&HardwareProfile::minimal());
        small.validate().unwrap();
        assert_eq!(small.memtable_bytes, 4 << 20);
        assert_eq!(small.compaction_threads, 1);
        let big = HardwareProfile {
            page_size: 16_384,
            cache_line: 128,
            logical_cpus: 14,
            performance_cores: 10,
            efficiency_cores: 4,
            memory_bytes: 36 << 30,
        };
        let options = Options::for_hardware(&big);
        options.validate().unwrap();
        assert_eq!(
            (
                options.memtable_bytes,
                options.block_bytes,
                options.compaction_threads
            ),
            (64 << 20, 16_384, 4)
        );
        assert_eq!(options.level_bytes(3), options.level1_bytes * 100);
    }
}
