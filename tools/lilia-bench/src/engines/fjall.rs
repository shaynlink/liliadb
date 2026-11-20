//! fjall: LSM tree in Rust; every batch commits with `PersistMode::SyncAll`.

use std::ops::Bound;
use std::path::Path;

use fjall::{KeyspaceCreateOptions, PersistMode, Readable};

use super::{std_file_sync, Engine, Result, Session};
use crate::record::EngineInfo;

pub struct Fjall {
    db: fjall::Database,
    kv: fjall::Keyspace,
}

impl std::fmt::Debug for Fjall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Fjall")
    }
}

impl Fjall {
    pub fn open(dir: &Path) -> Result<Self> {
        let db = fjall::Database::builder(dir.join("fjall")).open()?;
        let kv = db.keyspace("kv", KeyspaceCreateOptions::default)?;
        Ok(Self { db, kv })
    }
}

impl Engine for Fjall {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            id: "fjall".into(),
            name: "fjall".into(),
            version: env!("LILIA_BENCH_DEP_FJALL").into(),
            sync: std_file_sync(),
        }
    }

    fn session(&self) -> Result<Box<dyn Session + '_>> {
        Ok(Box::new(FjallSession(self)))
    }
}

struct FjallSession<'a>(&'a Fjall);

impl Session for FjallSession<'_> {
    fn put(&mut self, items: &[(&[u8], &[u8])]) -> Result<()> {
        let mut batch = self.0.db.batch().durability(Some(PersistMode::SyncAll));
        for (k, v) in items {
            batch.insert(&self.0.kv, *k, *v);
        }
        batch.commit()?;
        Ok(())
    }

    fn delete(&mut self, keys: &[&[u8]]) -> Result<()> {
        let mut batch = self.0.db.batch().durability(Some(PersistMode::SyncAll));
        for k in keys {
            batch.remove(&self.0.kv, *k);
        }
        batch.commit()?;
        Ok(())
    }

    fn get(&mut self, key: &[u8]) -> Result<Option<usize>> {
        Ok(self.0.kv.get(key)?.map(|v| v.len()))
    }

    fn multi_get(&mut self, keys: &[&[u8]]) -> Result<usize> {
        let snapshot = self.0.db.snapshot();
        let mut found = 0;
        for key in keys {
            found += usize::from(snapshot.get(&self.0.kv, key)?.is_some());
        }
        Ok(found)
    }

    fn range_after(&mut self, key: &[u8], limit: usize) -> Result<usize> {
        let mut count = 0;
        for guard in self.0.kv.range::<&[u8], _>((Bound::Excluded(key), Bound::Unbounded)).take(limit) {
            count += usize::from(!guard.value()?.is_empty());
        }
        Ok(count)
    }

    fn scan_all(&mut self) -> Result<u64> {
        let mut count = 0;
        for guard in self.0.kv.iter() {
            count += u64::from(!guard.value()?.is_empty());
        }
        Ok(count)
    }
}
