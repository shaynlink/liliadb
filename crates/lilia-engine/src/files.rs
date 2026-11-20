//! Names of the files of a database directory, and the store that opens and deletes tables.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lilia_io::{OpenMode, Vfs};
use parking_lot::Mutex;

use crate::error::Result;
use crate::table::cache::BlockCache;
use crate::table::Table;

pub const CURRENT: &str = "CURRENT";
pub const LOCK: &str = "LOCK";

/// A file of the database directory, recognised by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Journal(u64),
    Table(u64),
    Manifest(u64),
    Temporary(u64),
}

pub fn journal(dir: &Path, number: u64) -> PathBuf {
    dir.join(format!("{number:06}.log"))
}

pub fn table(dir: &Path, number: u64) -> PathBuf {
    dir.join(format!("{number:06}.sst"))
}

pub fn manifest(dir: &Path, number: u64) -> PathBuf {
    dir.join(format!("MANIFEST-{number:06}"))
}

pub fn temporary(dir: &Path, number: u64) -> PathBuf {
    dir.join(format!("{number:06}.tmp"))
}

pub fn parse(name: &str) -> Option<FileKind> {
    if let Some(n) = name.strip_prefix("MANIFEST-") {
        return n.parse().ok().map(FileKind::Manifest);
    }
    let (stem, extension) = name.split_once('.')?;
    let number = stem.parse().ok()?;
    match extension {
        "log" => Some(FileKind::Journal(number)),
        "sst" => Some(FileKind::Table(number)),
        "tmp" => Some(FileKind::Temporary(number)),
        _ => None,
    }
}

/// Opens tables on demand, keeps them open, and deletes their files.
#[derive(Debug)]
pub struct TableStore {
    pub vfs: Arc<dyn Vfs>,
    pub dir: PathBuf,
    pub cache: Arc<BlockCache>,
    open: Mutex<HashMap<u64, Arc<Table>>>,
}

impl TableStore {
    pub fn new(vfs: Arc<dyn Vfs>, dir: PathBuf, cache: Arc<BlockCache>) -> Self {
        Self { vfs, dir, cache, open: Mutex::new(HashMap::new()) }
    }

    pub fn get(&self, number: u64, size: u64) -> Result<Arc<Table>> {
        if let Some(table) = self.open.lock().get(&number) {
            return Ok(Arc::clone(table));
        }
        let file = self.vfs.open(&table(&self.dir, number), OpenMode::Read)?;
        let opened = Arc::new(Table::open(number, file, size, Arc::clone(&self.cache))?);
        Ok(Arc::clone(self.open.lock().entry(number).or_insert(opened)))
    }

    /// Forgets table `number` and deletes its file. A failure leaves an orphan file, which the
    /// next open of the database deletes.
    pub fn remove(&self, number: u64) -> std::io::Result<()> {
        self.open.lock().remove(&number);
        self.cache.evict_table(number);
        self.vfs.remove_file(&table(&self.dir, number))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        let dir = Path::new("/db");
        for (path, kind) in [
            (journal(dir, 7), FileKind::Journal(7)),
            (table(dir, 12), FileKind::Table(12)),
            (manifest(dir, 3), FileKind::Manifest(3)),
            (temporary(dir, 9), FileKind::Temporary(9)),
        ] {
            assert_eq!(parse(path.file_name().unwrap().to_str().unwrap()), Some(kind));
        }
        assert_eq!(parse("CURRENT"), None);
        assert_eq!(parse("notes.txt"), None);
    }
}
