//! LiliaDB through its public embedded Rust API.

use std::path::Path;
use std::sync::Arc;

use lilia_core::{Aes256GcmCodec, BatchOperation};
use lilia_storage_sqlite::{Database, DatabaseOptions};

use super::{Engine, Result, Session, SyncPrimitive};
use crate::record::EngineInfo;

/// `lilia`: defaults (`Durability::Durable`, ledger on). `lilia-noledger`: ledger off.
/// `lilia-aes`: defaults plus AES-256-GCM value encryption.
pub const IDS: [&str; 3] = ["lilia", "lilia-noledger", "lilia-aes"];
const NAMESPACE: &str = "bench";
const MAX_BATCH: usize = 1_000;

#[derive(Debug)]
pub struct Lilia {
    id: String,
    db: Database,
}

impl Lilia {
    pub fn open(id: &str, dir: &Path) -> Result<Self> {
        let mut options = DatabaseOptions::durable(dir.join("bench.lilia"));
        match id {
            "lilia-noledger" => options = options.with_ledger(false),
            "lilia-aes" => {
                options = options.with_codec(Arc::new(Aes256GcmCodec::from_passphrase("lilia-bench-key")));
            }
            _ => {}
        }
        Ok(Self { id: id.to_owned(), db: Database::open(options)? })
    }

    /// The database, for the model workloads (JSON, full-text, vectors, analytics).
    #[must_use]
    pub const fn database(&self) -> &Database {
        &self.db
    }
}

#[must_use]
pub fn info(id: &str) -> EngineInfo {
    let name = match id {
        "lilia-noledger" => "LiliaDB (ledger off)",
        "lilia-aes" => "LiliaDB (AES-256-GCM)",
        _ => "LiliaDB",
    };
    EngineInfo {
        id: id.to_owned(),
        name: name.to_owned(),
        version: env!("LILIA_BENCH_LILIA_VERSION").to_owned(),
        // `Durability::Durable` runs SQLite with `synchronous=FULL` and without `fullfsync`.
        sync: SyncPrimitive::Fsync,
    }
}

impl Engine for Lilia {
    fn info(&self) -> EngineInfo {
        info(&self.id)
    }

    fn session(&self) -> Result<Box<dyn Session + '_>> {
        Ok(Box::new(LiliaSession(&self.db)))
    }
}

struct LiliaSession<'a>(&'a Database);

impl Session for LiliaSession<'_> {
    fn put(&mut self, items: &[(&[u8], &[u8])]) -> Result<()> {
        // `batch` accepts at most 1,000 operations; larger atomic writes go through the
        // transaction API, which is what an application has to use for them.
        if items.len() > MAX_BATCH {
            return Ok(self.0.with_transaction(|tx| {
                for (k, v) in items {
                    tx.kv().set(NAMESPACE, k, v)?;
                }
                Ok(())
            })?);
        }
        let ops: Vec<_> = items
            .iter()
            .map(|(k, v)| BatchOperation::KvSet {
                namespace: NAMESPACE.into(),
                key: k.to_vec(),
                value: v.to_vec(),
                if_version: None,
                expires_at_ms: None,
            })
            .collect();
        self.0.batch(&ops)?;
        Ok(())
    }

    fn delete(&mut self, keys: &[&[u8]]) -> Result<()> {
        let ops: Vec<_> = keys
            .iter()
            .map(|k| BatchOperation::KvDelete { namespace: NAMESPACE.into(), key: k.to_vec(), if_version: None })
            .collect();
        self.0.batch(&ops)?;
        Ok(())
    }

    fn get(&mut self, key: &[u8]) -> Result<Option<usize>> {
        Ok(self.0.kv_get(NAMESPACE, key)?.map(|e| e.value.len()))
    }

    fn multi_get(&mut self, keys: &[&[u8]]) -> Result<usize> {
        // The public API has no multi-key read; this is what an application has to write.
        let mut found = 0;
        for key in keys {
            found += usize::from(self.0.kv_get(NAMESPACE, key)?.is_some());
        }
        Ok(found)
    }

    fn range_after(&mut self, key: &[u8], limit: usize) -> Result<usize> {
        Ok(self.0.kv_scan(NAMESPACE, Some(key), u32::try_from(limit)?)?.len())
    }

    fn scan_all(&mut self) -> Result<u64> {
        let mut count = 0u64;
        let mut after: Option<Vec<u8>> = None;
        loop {
            let page = self.0.kv_scan(NAMESPACE, after.as_deref(), 1_000)?;
            let Some(last) = page.last() else { return Ok(count) };
            count += page.len() as u64;
            after = Some(last.key.clone());
        }
    }
}
