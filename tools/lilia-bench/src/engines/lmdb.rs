//! LMDB through `heed`: memory-mapped B+tree, default (fully synced) environment flags.

use std::ops::Bound;
use std::path::Path;

use heed::types::Bytes;
use heed::{CommitOrAbort, EnvOpenOptions};

use super::{std_file_sync, Engine, Result, Session};
use crate::record::EngineInfo;

/// Upper bound of the memory map; LMDB reserves address space, not memory.
const MAP_SIZE: usize = 64 << 30;

type Db<'e> = heed::Database<'e, Bytes, Bytes>;

/// Owns the environment, so dropping the engine closes it and the directory can be reopened.
pub struct Lmdb {
    env: heed::Env,
}

impl std::fmt::Debug for Lmdb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Lmdb")
    }
}

impl Lmdb {
    #[allow(
        unsafe_code,
        reason = "heed marks `open` unsafe; this process opens the directory once at a time"
    )]
    pub fn open(dir: &Path) -> Result<Self> {
        let path = dir.join("lmdb");
        std::fs::create_dir_all(&path)?;
        // SAFETY: no other process opens this directory, and this process closes the previous
        // environment (by dropping its engine) before reopening it.
        let env = unsafe { EnvOpenOptions::new().map_size(MAP_SIZE).max_dbs(1).open(&path)? };
        let wtxn = env.write_txn()?;
        env.create_databases_and_commit(wtxn, |opener, wtxn| {
            opener.create_database::<Bytes, Bytes>(wtxn, None).map(|_| CommitOrAbort::<(), ()>::Commit(()))
        })?;
        Ok(Self { env })
    }

    fn database(&self) -> Result<Db<'_>> {
        let rtxn = self.env.read_txn()?;
        self.env
            .open_databases_and_commit(rtxn, |opener, rtxn| -> std::result::Result<_, heed::Error> {
                Ok(CommitOrAbort::<_, ()>::Commit(opener.open_database::<Bytes, Bytes>(rtxn, None)?))
            })?
            .unwrap_commit()
            .ok_or_else(|| "the unnamed LMDB database is missing".into())
    }
}

impl Engine for Lmdb {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            id: "lmdb".into(),
            name: "LMDB".into(),
            version: format!(
                "lmdb-master-sys {} (heed {})",
                env!("LILIA_BENCH_DEP_LMDB_SYS"),
                env!("LILIA_BENCH_DEP_HEED")
            ),
            // `mdb.c` maps its data sync to `fcntl(F_FULLFSYNC)` on Apple platforms.
            sync: std_file_sync(),
        }
    }

    fn session(&self) -> Result<Box<dyn Session + '_>> {
        Ok(Box::new(LmdbSession { env: &self.env, db: self.database()? }))
    }
}

struct LmdbSession<'a> {
    env: &'a heed::Env,
    db: Db<'a>,
}

impl Session for LmdbSession<'_> {
    fn put(&mut self, items: &[(&[u8], &[u8])]) -> Result<()> {
        let mut tx = self.env.write_txn()?;
        for (k, v) in items {
            self.db.put(&mut tx, k, v)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn delete(&mut self, keys: &[&[u8]]) -> Result<()> {
        let mut tx = self.env.write_txn()?;
        for k in keys {
            self.db.delete(&mut tx, k)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn get(&mut self, key: &[u8]) -> Result<Option<usize>> {
        let tx = self.env.read_txn()?;
        Ok(self.db.get(&tx, key)?.map(<[u8]>::len))
    }

    fn multi_get(&mut self, keys: &[&[u8]]) -> Result<usize> {
        let tx = self.env.read_txn()?;
        let mut found = 0;
        for key in keys {
            found += usize::from(self.db.get(&tx, key)?.is_some());
        }
        Ok(found)
    }

    fn range_after(&mut self, key: &[u8], limit: usize) -> Result<usize> {
        let tx = self.env.read_txn()?;
        let bounds: (Bound<&[u8]>, Bound<&[u8]>) = (Bound::Excluded(key), Bound::Unbounded);
        let mut count = 0;
        for entry in self.db.range(&tx, &bounds)?.take(limit) {
            count += usize::from(!entry?.1.is_empty());
        }
        Ok(count)
    }

    fn scan_all(&mut self) -> Result<u64> {
        let tx = self.env.read_txn()?;
        let mut count = 0;
        for entry in self.db.iter(&tx)? {
            count += u64::from(!entry?.1.is_empty());
        }
        Ok(count)
    }
}
