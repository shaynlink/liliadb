//! RocksDB: LSM tree in C++, every write batch committed with `WriteOptions::set_sync(true)`.
//!
//! The `rocksdb` crate builds RocksDB without `HAVE_FULLFSYNC` on macOS, so its sync is a plain
//! `fsync`. The suite builds a second binary with `-DHAVE_FULLFSYNC` (reported as
//! `rocksdb-fullfsync`) to measure it in the power-safe class.

use std::path::Path;

use rocksdb::{Direction, IteratorMode, Options, WriteBatch, WriteOptions, DB};

use super::{Engine, Result, Session, SyncPrimitive};
use crate::record::EngineInfo;

const FULLFSYNC: bool = matches!(env!("LILIA_BENCH_ROCKSDB_FULLFSYNC").as_bytes(), b"1");
pub const ID: &str = if FULLFSYNC { "rocksdb-fullfsync" } else { "rocksdb" };

#[derive(Debug)]
pub struct Rocks(DB);

impl Rocks {
    pub fn open(dir: &Path) -> Result<Self> {
        let mut options = Options::default();
        options.create_if_missing(true);
        options.increase_parallelism(i32::try_from(std::thread::available_parallelism().map_or(4, usize::from))?);
        Ok(Self(DB::open(&options, dir.join("rocksdb"))?))
    }
}

impl Engine for Rocks {
    fn info(&self) -> EngineInfo {
        let rocks = env!("LILIA_BENCH_DEP_LIBROCKSDB_SYS").split('+').nth(1).unwrap_or("unknown");
        EngineInfo {
            id: ID.into(),
            name: if FULLFSYNC { "RocksDB (HAVE_FULLFSYNC)" } else { "RocksDB" }.into(),
            version: format!("{rocks} (rocksdb crate {})", env!("LILIA_BENCH_DEP_ROCKSDB")),
            sync: if FULLFSYNC && cfg!(any(target_os = "macos", target_os = "ios")) {
                SyncPrimitive::FullFsync
            } else if cfg!(windows) {
                SyncPrimitive::FlushFileBuffers
            } else {
                SyncPrimitive::Fsync
            },
        }
    }

    fn session(&self) -> Result<Box<dyn Session + '_>> {
        Ok(Box::new(RocksSession(&self.0)))
    }
}

struct RocksSession<'a>(&'a DB);

fn synced() -> WriteOptions {
    let mut options = WriteOptions::default();
    options.set_sync(true);
    options
}

impl Session for RocksSession<'_> {
    fn put(&mut self, items: &[(&[u8], &[u8])]) -> Result<()> {
        let mut batch = WriteBatch::default();
        for (k, v) in items {
            batch.put(k, v);
        }
        self.0.write_opt(batch, &synced())?;
        Ok(())
    }

    fn delete(&mut self, keys: &[&[u8]]) -> Result<()> {
        let mut batch = WriteBatch::default();
        for k in keys {
            batch.delete(k);
        }
        self.0.write_opt(batch, &synced())?;
        Ok(())
    }

    fn get(&mut self, key: &[u8]) -> Result<Option<usize>> {
        Ok(self.0.get_pinned(key)?.map(|v| v.len()))
    }

    fn multi_get(&mut self, keys: &[&[u8]]) -> Result<usize> {
        let mut found = 0;
        for value in self.0.multi_get(keys) {
            found += usize::from(value?.is_some());
        }
        Ok(found)
    }

    fn range_after(&mut self, key: &[u8], limit: usize) -> Result<usize> {
        let mut count = 0;
        for entry in self.0.iterator(IteratorMode::From(key, Direction::Forward)) {
            let (k, v) = entry?;
            if &*k == key {
                continue;
            }
            count += usize::from(!v.is_empty());
            if count == limit {
                break;
            }
        }
        Ok(count)
    }

    fn scan_all(&mut self) -> Result<u64> {
        let mut count = 0;
        for entry in self.0.iterator(IteratorMode::Start) {
            count += u64::from(!entry?.1.is_empty());
        }
        Ok(count)
    }
}
