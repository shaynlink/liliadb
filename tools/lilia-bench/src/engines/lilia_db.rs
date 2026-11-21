//! `lilia-db`: the v0.2 data-model layer over `lilia-engine`, through its key-value model, to
//! measure what the model layer (record envelopes, planning, the ledger) costs over the engine.

use std::path::Path;

use lilia_db::{Batch, Database, Options, Scan};
use lilia_engine::Durability;

use super::{std_file_sync, Engine, Result, Session};
use crate::record::EngineInfo;

/// `lilia-db`: no ledger. `lilia-db-ledger`: every commit appended to the ledger. Both durable.
pub const IDS: [&str; 2] = ["lilia-db", "lilia-db-ledger"];

const NAMESPACE: &str = "bench";

#[derive(Debug)]
pub struct LiliaDb {
    id: String,
    db: Database,
}

impl LiliaDb {
    pub fn open(id: &str, dir: &Path) -> Result<Self> {
        let mut options = Options { ledger: id == "lilia-db-ledger", ..Options::default() };
        options.engine.durability = Durability::Durable;
        let db = Database::open(dir.join("db"), options)?;
        Ok(Self { id: id.to_owned(), db })
    }
}

impl Engine for LiliaDb {
    fn info(&self) -> EngineInfo {
        let ledger = self.id == "lilia-db-ledger";
        EngineInfo {
            id: self.id.clone(),
            name: if ledger { "lilia-db (v0.2, Durable, ledger)" } else { "lilia-db (v0.2, Durable)" }.into(),
            version: env!("LILIA_BENCH_LILIA_VERSION").split(' ').next().unwrap_or("unknown").to_owned(),
            sync: std_file_sync(),
        }
    }

    fn session(&self) -> Result<Box<dyn Session + '_>> {
        Ok(Box::new(DbSession(&self.db)))
    }
}

struct DbSession<'a>(&'a Database);

impl Session for DbSession<'_> {
    fn put(&mut self, items: &[(&[u8], &[u8])]) -> Result<()> {
        let mut batch = Batch::new();
        for (k, v) in items {
            batch.kv_put(NAMESPACE, *k, *v);
        }
        self.0.system().commit(&batch)?;
        Ok(())
    }

    fn delete(&mut self, keys: &[&[u8]]) -> Result<()> {
        let mut batch = Batch::new();
        for k in keys {
            batch.kv_delete(NAMESPACE, *k);
        }
        self.0.system().commit(&batch)?;
        Ok(())
    }

    fn get(&mut self, key: &[u8]) -> Result<Option<usize>> {
        Ok(self.0.system().kv_get(NAMESPACE, key)?.map(|e| e.value.len()))
    }

    fn multi_get(&mut self, keys: &[&[u8]]) -> Result<usize> {
        let tx = self.0.system().transaction();
        let mut found = 0;
        for key in keys {
            found += usize::from(tx.kv_get(NAMESPACE, key)?.is_some());
        }
        Ok(found)
    }

    fn range_after(&mut self, key: &[u8], limit: usize) -> Result<usize> {
        let entries = self.0.system().kv_scan(NAMESPACE, &Scan::all().after(key).limit(limit))?;
        Ok(entries.iter().filter(|e| !e.value.is_empty()).count())
    }

    fn scan_all(&mut self) -> Result<u64> {
        let entries = self.0.system().kv_scan(NAMESPACE, &Scan::all())?;
        Ok(entries.iter().filter(|e| !e.value.is_empty()).count() as u64)
    }
}
