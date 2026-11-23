//! Ordered key-value engines under test, behind one minimal interface.
//!
//! Every engine runs in its most durable configuration for the class it declares: a `put` returns
//! only once the commit has gone through the engine's sync primitive ([`SyncPrimitive`]).

use std::path::Path;

use crate::record::EngineInfo;
#[allow(unused_imports)] // reason: only the adapters compiled into this binary name it.
use crate::record::SyncPrimitive;

#[cfg(feature = "kvs")]
mod fjall;
#[cfg(feature = "lilia")]
pub mod lilia_db;
#[cfg(feature = "lilia")]
mod lilia_engine;
#[cfg(feature = "kvs")]
mod lmdb;
#[cfg(feature = "kvs")]
mod redb;
#[cfg(feature = "rocksdb")]
mod rocksdb;
#[cfg(feature = "sqlite")]
pub mod sqlite;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

/// An open database. Shared by every worker thread of a workload.
pub trait Engine: Send + Sync {
    fn info(&self) -> EngineInfo;
    /// A handle for one thread. Engines whose handles are not thread-safe (SQLite) open a
    /// connection per session; the others borrow the shared handle.
    fn session(&self) -> Result<Box<dyn Session + '_>>;
}

/// Operations of one thread. Every mutating call is one atomic, synced commit.
pub trait Session {
    fn put(&mut self, items: &[(&[u8], &[u8])]) -> Result<()>;
    fn delete(&mut self, keys: &[&[u8]]) -> Result<()>;
    /// Value length when the key exists.
    fn get(&mut self, key: &[u8]) -> Result<Option<usize>>;
    /// Number of keys found, read from one consistent snapshot.
    fn multi_get(&mut self, keys: &[&[u8]]) -> Result<usize>;
    /// Reads up to `limit` entries with keys strictly greater than `key`, in order.
    fn range_after(&mut self, key: &[u8], limit: usize) -> Result<usize>;
    /// Reads every entry in key order; returns the entry count.
    fn scan_all(&mut self) -> Result<u64>;
}

/// Engine identifiers compiled into this binary.
#[must_use]
pub fn available() -> Vec<&'static str> {
    let groups: &[&[&'static str]] = &[
        #[cfg(feature = "lilia")]
        &lilia_engine::IDS,
        #[cfg(feature = "lilia")]
        &lilia_db::IDS,
        #[cfg(feature = "sqlite")]
        &sqlite::IDS,
        #[cfg(feature = "kvs")]
        &["redb", "fjall", "lmdb"],
        #[cfg(feature = "rocksdb")]
        &[rocksdb::ID],
    ];
    groups.concat()
}

/// Opens (creating) engine `id` in the empty or previously used directory `dir`.
pub fn open(id: &str, dir: &Path) -> Result<Box<dyn Engine>> {
    #[cfg(feature = "lilia")]
    if lilia_engine::IDS.contains(&id) {
        return Ok(Box::new(lilia_engine::LiliaEngine::open(id, dir)?));
    }
    #[cfg(feature = "lilia")]
    if lilia_db::IDS.contains(&id) {
        return Ok(Box::new(lilia_db::LiliaDb::open(id, dir)?));
    }
    #[cfg(feature = "sqlite")]
    if sqlite::IDS.contains(&id) {
        return Ok(Box::new(sqlite::Sqlite::open(id, dir)?));
    }
    #[cfg(feature = "kvs")]
    match id {
        "redb" => return Ok(Box::new(redb::Redb::open(dir)?)),
        "fjall" => return Ok(Box::new(fjall::Fjall::open(dir)?)),
        "lmdb" => return Ok(Box::new(lmdb::Lmdb::open(dir)?)),
        _ => {}
    }
    #[cfg(feature = "rocksdb")]
    if id == rocksdb::ID {
        return Ok(Box::new(rocksdb::Rocks::open(dir)?));
    }
    Err(format!(
        "engine `{id}` is not compiled into this binary (available: {:?}); cannot open {}",
        available(),
        dir.display()
    )
    .into())
}

/// Sync primitive of engines that sync through Rust's `File::sync_all`/`sync_data`, which issue
/// `F_FULLFSYNC` on Apple platforms.
#[allow(dead_code)] // reason: used only by the adapters compiled into this binary.
#[must_use]
pub fn std_file_sync() -> SyncPrimitive {
    if cfg!(any(target_os = "macos", target_os = "ios")) {
        SyncPrimitive::FullFsync
    } else if cfg!(windows) {
        SyncPrimitive::FlushFileBuffers
    } else {
        SyncPrimitive::Fsync
    }
}
