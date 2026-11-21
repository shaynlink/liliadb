//! `lilia-engine`: the v0.2 native storage engine, through its own API (not yet behind the
//! `LiliaDB` model layer).

use std::ops::Bound;
use std::path::Path;

use lilia_engine::{Db, Durability, Options, WriteBatch};

use super::{std_file_sync, Engine, Result, Session, SyncPrimitive};
use crate::record::EngineInfo;

/// `lilia-engine`: `Durability::Durable`. `lilia-engine-balanced`: `Durability::Balanced`.
pub const IDS: [&str; 2] = ["lilia-engine", "lilia-engine-balanced"];

#[derive(Debug)]
pub struct LiliaEngine {
    id: String,
    db: Db,
}

impl LiliaEngine {
    pub fn open(id: &str, dir: &Path) -> Result<Self> {
        let durability = if id == "lilia-engine-balanced" { Durability::Balanced } else { Durability::Durable };
        let db = Db::open(dir.join("engine"), Options { durability, ..Options::default() })?;
        Ok(Self { id: id.to_owned(), db })
    }
}

impl Engine for LiliaEngine {
    fn info(&self) -> EngineInfo {
        let balanced = self.id == "lilia-engine-balanced";
        EngineInfo {
            id: self.id.clone(),
            name: if balanced { "lilia-engine (v0.2, Balanced)" } else { "lilia-engine (v0.2, Durable)" }.into(),
            version: env!("LILIA_BENCH_LILIA_VERSION").split(' ').next().unwrap_or("unknown").to_owned(),
            sync: if balanced && cfg!(any(target_os = "macos", target_os = "ios")) { SyncPrimitive::Barrier } else { std_file_sync() },
        }
    }

    fn session(&self) -> Result<Box<dyn Session + '_>> {
        Ok(Box::new(EngineSession(&self.db)))
    }
}

struct EngineSession<'a>(&'a Db);

impl Session for EngineSession<'_> {
    fn put(&mut self, items: &[(&[u8], &[u8])]) -> Result<()> {
        let mut batch = WriteBatch::new();
        for (k, v) in items {
            batch.put(k.to_vec(), v.to_vec());
        }
        self.0.write(batch)?;
        Ok(())
    }

    fn delete(&mut self, keys: &[&[u8]]) -> Result<()> {
        let mut batch = WriteBatch::new();
        for k in keys {
            batch.delete(k.to_vec());
        }
        self.0.write(batch)?;
        Ok(())
    }

    fn get(&mut self, key: &[u8]) -> Result<Option<usize>> {
        Ok(self.0.get(key)?.map(|v| v.len()))
    }

    fn multi_get(&mut self, keys: &[&[u8]]) -> Result<usize> {
        let snapshot = self.0.snapshot();
        let mut found = 0;
        for key in keys {
            found += usize::from(self.0.get_at(&snapshot, key)?.is_some());
        }
        Ok(found)
    }

    fn range_after(&mut self, key: &[u8], limit: usize) -> Result<usize> {
        let mut count = 0;
        for entry in self.0.range(Bound::Excluded(key), Bound::Unbounded)?.take(limit) {
            count += usize::from(!entry?.value.is_empty());
        }
        Ok(count)
    }

    fn scan_all(&mut self) -> Result<u64> {
        let mut count = 0;
        for entry in self.0.range(Bound::Unbounded, Bound::Unbounded)? {
            count += u64::from(!entry?.value.is_empty());
        }
        Ok(count)
    }
}
