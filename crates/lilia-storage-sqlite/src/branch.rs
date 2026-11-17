use std::ops::Deref;
use std::sync::{Arc, Mutex};

use lilia_core::{BranchDiff, Result};
use lilia_sqlite_native::{BranchVfsState, NativeSession};

use crate::Database;

/// An isolated, zero-cost (O(1)) Copy-on-Write virtual branch of a database.
///
/// Shares the parent database's pages in memory/disk with 0 extra bytes at creation time.
/// All mutations performed within the branch are held in an in-memory page overlay and
/// a native SQLite session changeset, without ever touching the parent database.
pub struct Branch {
    pub(crate) name: String,
    pub(crate) db: Database,
    pub(crate) session: Mutex<Option<NativeSession>>,
    pub(crate) cow_state: Arc<BranchVfsState>,
    pub(crate) created_at_ms: i64,
}

impl std::fmt::Debug for Branch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Branch")
            .field("name", &self.name)
            .field("created_at_ms", &self.created_at_ms)
            .field("dirty_pages", &self.cow_state.dirty_page_count())
            .field("memory_bytes", &self.cow_state.memory_bytes())
            .finish()
    }
}

impl Deref for Branch {
    type Target = Database;

    fn deref(&self) -> &Database {
        &self.db
    }
}

impl Branch {
    /// Return the branch name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Number of SQLite database pages modified in memory by this branch.
    #[must_use]
    pub fn dirty_page_count(&self) -> usize {
        self.cow_state.dirty_page_count()
    }

    /// Total memory in bytes consumed by modified pages in this branch.
    #[must_use]
    pub fn memory_bytes(&self) -> usize {
        self.cow_state.memory_bytes()
    }

    /// Return the timestamp in milliseconds when this branch was created.
    #[must_use]
    pub fn created_at_ms(&self) -> i64 {
        self.created_at_ms
    }

    /// Generate the binary changeset recording all mutations made in this branch.
    pub fn changeset(&self) -> Result<Vec<u8>> {
        let lock = self.session.lock().unwrap();
        if let Some(ref sess) = *lock {
            sess.changeset()
        } else {
            Ok(Vec::new())
        }
    }

    /// Generate the compact patchset recording all primary keys and modified column values.
    pub fn patchset(&self) -> Result<Vec<u8>> {
        let lock = self.session.lock().unwrap();
        if let Some(ref sess) = *lock {
            sess.patchset()
        } else {
            Ok(Vec::new())
        }
    }

    /// Compute summary statistics of changes made in this branch.
    pub fn diff(&self) -> Result<BranchDiff> {
        let _ = self.changeset()?;
        Ok(BranchDiff {
            branch_name: self.name.clone(),
            total_changes: self.dirty_page_count(),
            tables: Vec::new(),
        })
    }
}

impl Drop for Branch {
    fn drop(&mut self) {
        drop(self.session.lock().unwrap().take());
        lilia_sqlite_native::unregister_branch_state(&self.name);
    }
}
