//! SQLite (bundled amalgamation through `rusqlite`), WAL, `synchronous=FULL`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};

use super::{Engine, Result, Session, SyncPrimitive};
use crate::record::EngineInfo;

/// `sqlite`: the stock durable configuration. `sqlite-fullfsync`: adds `fullfsync` and
/// `checkpoint_fullfsync`, which make commits power-safe on Apple platforms.
pub const IDS: [&str; 2] = ["sqlite", "sqlite-fullfsync"];

#[derive(Debug)]
pub struct Sqlite {
    id: String,
    path: PathBuf,
}

impl Sqlite {
    pub fn open(id: &str, dir: &Path) -> Result<Self> {
        let engine = Self { id: id.to_owned(), path: dir.join("bench.sqlite") };
        engine.connect()?.execute_batch(
            "CREATE TABLE IF NOT EXISTS kv(k BLOB PRIMARY KEY, v BLOB NOT NULL) STRICT, WITHOUT ROWID;",
        )?;
        Ok(engine)
    }

    /// A configured connection; every session owns one, as SQLite applications do.
    pub fn connect(&self) -> Result<Connection> {
        let conn = Connection::open(&self.path)?;
        conn.busy_timeout(Duration::from_secs(60))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        let full = self.id == "sqlite-fullfsync";
        conn.pragma_update(None, "fullfsync", full)?;
        conn.pragma_update(None, "checkpoint_fullfsync", full)?;
        conn.pragma_update(None, "cache_size", -65_536)?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        Ok(conn)
    }
}

#[must_use]
pub fn info(id: &str) -> EngineInfo {
    let full = id == "sqlite-fullfsync";
    EngineInfo {
        id: id.to_owned(),
        name: if full { "SQLite (WAL, fullfsync)" } else { "SQLite (WAL)" }.to_owned(),
        version: format!("{} (rusqlite {})", rusqlite::version(), env!("LILIA_BENCH_DEP_RUSQLITE")),
        sync: if full && cfg!(any(target_os = "macos", target_os = "ios")) {
            SyncPrimitive::FullFsync
        } else if cfg!(windows) {
            SyncPrimitive::FlushFileBuffers
        } else {
            SyncPrimitive::Fsync
        },
    }
}

impl Engine for Sqlite {
    fn info(&self) -> EngineInfo {
        info(&self.id)
    }

    fn session(&self) -> Result<Box<dyn Session + '_>> {
        Ok(Box::new(SqliteSession(self.connect()?)))
    }
}

struct SqliteSession(Connection);

impl Session for SqliteSession {
    fn put(&mut self, items: &[(&[u8], &[u8])]) -> Result<()> {
        let tx = self.0.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        {
            let mut stmt =
                tx.prepare_cached("INSERT INTO kv(k, v) VALUES(?1, ?2) ON CONFLICT(k) DO UPDATE SET v = excluded.v")?;
            for (k, v) in items {
                stmt.execute(params![k, v])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn delete(&mut self, keys: &[&[u8]]) -> Result<()> {
        let tx = self.0.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        {
            let mut stmt = tx.prepare_cached("DELETE FROM kv WHERE k = ?1")?;
            for k in keys {
                stmt.execute([k])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn get(&mut self, key: &[u8]) -> Result<Option<usize>> {
        let mut stmt = self.0.prepare_cached("SELECT length(v) FROM kv WHERE k = ?1")?;
        let len: Option<i64> = stmt.query_row([key], |r| r.get(0)).optional()?;
        Ok(len.map(usize::try_from).transpose()?)
    }

    fn multi_get(&mut self, keys: &[&[u8]]) -> Result<usize> {
        let tx = self.0.transaction()?;
        let mut found = 0;
        {
            let mut stmt = tx.prepare_cached("SELECT v FROM kv WHERE k = ?1")?;
            for key in keys {
                found += usize::from(stmt.query_row([key], |r| Ok(r.get_ref(0)?.as_blob()?.len())).optional()?.is_some());
            }
        }
        tx.commit()?;
        Ok(found)
    }

    fn range_after(&mut self, key: &[u8], limit: usize) -> Result<usize> {
        let mut stmt = self.0.prepare_cached("SELECT k, v FROM kv WHERE k > ?1 ORDER BY k LIMIT ?2")?;
        let mut rows = stmt.query(params![key, i64::try_from(limit)?])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            count += usize::from(!row.get_ref(1)?.as_blob()?.is_empty());
        }
        Ok(count)
    }

    fn scan_all(&mut self) -> Result<u64> {
        let mut stmt = self.0.prepare_cached("SELECT k, v FROM kv ORDER BY k")?;
        let mut rows = stmt.query([])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            count += u64::from(!row.get_ref(1)?.as_blob()?.is_empty());
        }
        Ok(count)
    }
}
