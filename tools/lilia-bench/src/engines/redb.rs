//! redb: copy-on-write B-tree, `Durability::Immediate` (the default).

use std::ops::Bound;
use std::path::Path;

use redb::{ReadableDatabase, ReadableTable, TableDefinition};

use super::{std_file_sync, Engine, Result, Session};
use crate::record::EngineInfo;

const TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("kv");

#[derive(Debug)]
pub struct Redb(redb::Database);

impl Redb {
    pub fn open(dir: &Path) -> Result<Self> {
        let db = redb::Database::create(dir.join("bench.redb"))?;
        let tx = db.begin_write()?;
        tx.open_table(TABLE)?;
        tx.commit()?;
        Ok(Self(db))
    }
}

impl Engine for Redb {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            id: "redb".into(),
            name: "redb".into(),
            version: env!("LILIA_BENCH_DEP_REDB").into(),
            sync: std_file_sync(),
        }
    }

    fn session(&self) -> Result<Box<dyn Session + '_>> {
        Ok(Box::new(RedbSession(&self.0)))
    }
}

struct RedbSession<'a>(&'a redb::Database);

impl Session for RedbSession<'_> {
    fn put(&mut self, items: &[(&[u8], &[u8])]) -> Result<()> {
        let tx = self.0.begin_write()?;
        {
            let mut table = tx.open_table(TABLE)?;
            for (k, v) in items {
                table.insert(*k, *v)?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn delete(&mut self, keys: &[&[u8]]) -> Result<()> {
        let tx = self.0.begin_write()?;
        {
            let mut table = tx.open_table(TABLE)?;
            for k in keys {
                table.remove(*k)?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn get(&mut self, key: &[u8]) -> Result<Option<usize>> {
        let tx = self.0.begin_read()?;
        let table = tx.open_table(TABLE)?;
        let len = table.get(key)?.map(|v| v.value().len());
        Ok(len)
    }

    fn multi_get(&mut self, keys: &[&[u8]]) -> Result<usize> {
        let tx = self.0.begin_read()?;
        let table = tx.open_table(TABLE)?;
        let mut found = 0;
        for key in keys {
            found += usize::from(table.get(*key)?.is_some());
        }
        Ok(found)
    }

    fn range_after(&mut self, key: &[u8], limit: usize) -> Result<usize> {
        let tx = self.0.begin_read()?;
        let table = tx.open_table(TABLE)?;
        let mut count = 0;
        let bounds: (Bound<&[u8]>, Bound<&[u8]>) = (Bound::Excluded(key), Bound::Unbounded);
        for entry in table.range(bounds)?.take(limit) {
            count += usize::from(!entry?.1.value().is_empty());
        }
        Ok(count)
    }

    fn scan_all(&mut self) -> Result<u64> {
        let tx = self.0.begin_read()?;
        let table = tx.open_table(TABLE)?;
        let mut count = 0;
        for entry in table.iter()? {
            count += u64::from(!entry?.1.value().is_empty());
        }
        Ok(count)
    }
}
