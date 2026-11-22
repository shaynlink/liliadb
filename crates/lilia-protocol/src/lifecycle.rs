//! Opening a database from the options the daemon and the bindings take, sharing it between
//! calls, and closing it once they return.

#[cfg(any(unix, windows))]
use std::path::Path;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, Instant};

use lilia_core::{ErrorCode, LiliaError};
use lilia_db::{Database, Options};
use serde::Deserialize;

use crate::names;

/// How a daemon or a binding opens a database; names as [`names::durability`] and
/// [`names::profile`] read them. Its JSON form (for the C ABI) leaves out any field.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct OpenOptions {
    /// `"durable"` (default), `"balanced"` or `"performance"`.
    pub durability: Option<String>,
    /// `"server"` (default), `"mobile"` or `"constrained"`.
    pub profile: Option<String>,
    /// Keep a tamper-evident ledger of every record write. A database that keeps one must
    /// always be opened with it.
    pub ledger: bool,
    /// How long old versions stay readable, in milliseconds; a week by default, 0 keeps none.
    pub history_retention_ms: Option<u64>,
    /// Commits that can be undone; 0 (the default) makes commits not undoable.
    pub undo_depth: u32,
}

impl OpenOptions {
    /// The database options these describe.
    ///
    /// # Errors
    ///
    /// An unknown durability or profile name.
    pub fn options(&self) -> Result<Options, LiliaError> {
        let mut options = Options::for_profile(names::profile(self.profile.as_deref())?);
        options.engine.durability = names::durability(self.durability.as_deref())?;
        options.ledger = self.ledger;
        if let Some(retention) = self.history_retention_ms {
            options.history_retention = Duration::from_millis(retention);
        }
        options.undo_depth = self.undo_depth as usize;
        Ok(options)
    }
}

/// An open database that calls share and [`SharedDatabase::close`] closes: a binding's handle.
#[derive(Debug)]
pub struct SharedDatabase {
    inner: RwLock<Option<Arc<Database>>>,
}

impl SharedDatabase {
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self {
            inner: RwLock::new(Some(Arc::new(database))),
        }
    }

    /// The database, for one call.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::Closed`] once [`SharedDatabase::close`] or [`SharedDatabase::take`] ran.
    pub fn get(&self) -> Result<Arc<Database>, LiliaError> {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| LiliaError::new(ErrorCode::Closed, "the database is closed", false))
    }

    /// Takes the database out, so that later calls fail with [`ErrorCode::Closed`]; `None`
    /// when it was already taken. [`close`] closes what this returns.
    pub fn take(&self) -> Option<Arc<Database>> {
        self.inner
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Takes the database out and closes it once the calls holding it return; nothing when
    /// it was already closed.
    ///
    /// # Errors
    ///
    /// As [`close`].
    pub fn close(&self, timeout: Duration) -> Result<(), LiliaError> {
        self.take()
            .map_or(Ok(()), |database| close(database, timeout))
    }
}

/// Opens (or creates) the database of directory `path`, on the platform's file system (the
/// browser opens its databases on a pool of storage files instead: see `lilia-wasm`).
///
/// # Errors
///
/// Invalid options; the database's failure to open.
#[cfg(any(unix, windows))]
pub fn open(path: impl AsRef<Path>, options: &OpenOptions) -> Result<Database, LiliaError> {
    Ok(Database::open(path, options.options()?)?)
}

/// Closes `database`, making every acknowledged write durable, once the calls that still hold
/// it have returned, waiting for them at most `timeout`.
///
/// # Errors
///
/// [`ErrorCode::Timeout`] when calls still hold the database after `timeout`: it then closes
/// when the last of them returns, and nobody hears of a failure of that close. The failure of
/// the close otherwise.
pub fn close(database: Arc<Database>, timeout: Duration) -> Result<(), LiliaError> {
    // The clock is read only when calls hold the database: `wasm32-unknown-unknown`, whose
    // single thread leaves none, has no `Instant`.
    let mut database = match Arc::try_unwrap(database) {
        Ok(database) => return Ok(database.close()?),
        Err(shared) => shared,
    };
    let deadline = Instant::now() + timeout;
    loop {
        match Arc::try_unwrap(database) {
            Ok(database) => return Ok(database.close()?),
            Err(shared) if Instant::now() >= deadline => {
                let holders = Arc::strong_count(&shared) - 1;
                return Err(LiliaError::new(
                    ErrorCode::Timeout,
                    format!("{holders} calls still hold the database after {timeout:?}"),
                    true,
                ));
            }
            Err(shared) => {
                database = shared;
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_come_from_names_and_a_shared_database_closes_once_released() {
        let options = OpenOptions {
            durability: Some("performance".into()),
            profile: Some("constrained".into()),
            undo_depth: 4,
            ..OpenOptions::default()
        };
        let built = options.options().unwrap();
        assert_eq!(built.engine.durability, lilia_db::Durability::Performance);
        assert_eq!(built.undo_depth, 4);
        let unknown = OpenOptions {
            profile: Some("desktop".into()),
            ..OpenOptions::default()
        };
        assert_eq!(unknown.options().unwrap_err().code, ErrorCode::InvalidInput);

        let dir = tempfile::tempdir().unwrap();
        let database = Arc::new(open(dir.path(), &options).unwrap());
        let held = Arc::clone(&database);
        let error = close(Arc::clone(&database), Duration::from_millis(10)).unwrap_err();
        assert_eq!(error.code, ErrorCode::Timeout);
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            drop(held);
        });
        close(database, Duration::from_secs(5)).unwrap();
        releaser.join().unwrap();
    }
}
