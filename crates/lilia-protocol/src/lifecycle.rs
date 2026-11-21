//! Opening a database from the options the daemon and the bindings take, and closing a
//! database they share between calls.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lilia_core::{ErrorCode, LiliaError};
use lilia_db::{Database, Options};

use crate::names;

/// How a daemon or a binding opens a database; names as [`names::durability`] and
/// [`names::profile`] read them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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

/// Opens (or creates) the database of directory `path`.
///
/// # Errors
///
/// Invalid options; the database's failure to open.
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
pub fn close(mut database: Arc<Database>, timeout: Duration) -> Result<(), LiliaError> {
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
