//! Helpers shared by the integration tests.

// reason: each test crate compiles this module and uses a different subset of it.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::ops::Bound;

use lilia_engine::{Db, Durability, Options};
use lilia_io::HardwareProfile;

/// Tiny buffers so a few thousand writes exercise flushes, every level and compactions.
pub fn small_options(durability: Durability) -> Options {
    Options {
        durability,
        memtable_bytes: 16 << 10,
        max_immutable_memtables: 2,
        block_bytes: 512,
        bloom_bits_per_key: 10,
        block_cache_bytes: 256 << 10,
        l0_compaction_trigger: 2,
        l0_stop_writes: 8,
        level1_bytes: 48 << 10,
        level_multiplier: 3,
        target_file_bytes: 16 << 10,
        max_group_bytes: 64 << 10,
        compaction_threads: 2,
        ..Options::for_hardware(&HardwareProfile::minimal())
    }
}

/// `SplitMix64`.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

pub fn key(i: u64) -> Vec<u8> {
    format!("key{i:05}").into_bytes()
}

/// Every live key and value, read through a full range scan.
pub fn scan(db: &Db) -> BTreeMap<Vec<u8>, Vec<u8>> {
    db.range(Bound::Unbounded, Bound::Unbounded)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.key.to_vec(), entry.value.to_vec())
        })
        .collect()
}
