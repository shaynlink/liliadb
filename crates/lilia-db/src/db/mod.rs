//! The database: opening, the commit loop, maintenance. Catalog administration is in
//! [`schema`], the ledger in [`proofs`].

mod proofs;
mod schema;

use std::collections::{BTreeSet, HashMap};
use std::ops::Bound;
use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use lilia_core::{LedgerVerifyMode, SecurityContext};
use lilia_engine::Db;
use lilia_io::{HardwareProfile, NativeVfs, Vfs};
use parking_lot::{Mutex, RwLock};

use crate::batch::{Outcome, Write};
use crate::catalog::{Catalog, Schema};
use crate::error::{corruption, invalid, Result};
use crate::fts::Vocabularies;
use crate::guard::Access;
use crate::keys::{self};
use crate::ledger::{Attempt, Ledger};
use crate::record::{decode_kv, now_ms};
use crate::session::Session;
use crate::vectors::VectorIndexes;
use crate::write::{Plan, Planner};

/// Version of this layer's on-disk layout ([`crate::keys`], [`crate::record`]).
const FORMAT_VERSION: u8 = 1;
/// Documents indexed, or index entries deleted, per engine batch while building or dropping an
/// index.
pub(crate) const MAINTENANCE_CHUNK: usize = 256;

/// A document read by an index build: its key, version, id and index values.
pub(crate) type IndexedDoc = (Bytes, u64, String, BTreeSet<String>);

/// Database configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub engine: lilia_engine::Options,
    /// Keep a ledger of every record write ([`crate::ledger`]). A database that keeps a ledger
    /// must always be opened with this set: a ledger cannot be turned off.
    pub ledger: bool,
    /// Verification of the ledger when the database opens; an invalid ledger fails the open.
    pub verify_ledger: LedgerVerifyMode,
}

impl Options {
    /// Engine buffers sized from `hardware`, no ledger.
    #[must_use]
    pub fn for_hardware(hardware: &HardwareProfile) -> Self {
        Self {
            engine: lilia_engine::Options::for_hardware(hardware),
            ledger: false,
            verify_ledger: LedgerVerifyMode::None,
        }
    }
}

impl Default for Options {
    fn default() -> Self {
        Self::for_hardware(&HardwareProfile::probe())
    }
}

/// An open database. Every method takes `&self`; share it across threads by reference or `Arc`.
///
/// Reads and writes go through a [`Session`], which carries the caller's identity: row-level
/// security policies and masking rules apply to every session but [`Database::system`].
#[derive(Debug)]
pub struct Database {
    engine: Db,
    /// Writers hold the read lock from planning to commit, so a catalog change (which takes the
    /// write lock) never lands between a plan and its commit.
    catalog: RwLock<Catalog>,
    ledger: Option<Ledger>,
    /// Serializes index builds and drops, which run outside the catalog lock.
    admin: Mutex<()>,
    /// Full-text vocabularies loaded so far.
    vocabularies: Vocabularies,
    /// HNSW indexes built so far.
    vector_indexes: VectorIndexes,
}

impl Database {
    /// Opens (creating if allowed) the database in `path` on the native file system.
    ///
    /// # Errors
    ///
    /// Engine errors, an unsupported layout, a ledger that cannot be opened or fails
    /// verification.
    pub fn open(path: impl AsRef<Path>, options: Options) -> Result<Self> {
        Self::open_with(Arc::new(NativeVfs), path, options)
    }

    /// Opens the database in `path` on `vfs`.
    ///
    /// # Errors
    ///
    /// As [`Database::open`].
    pub fn open_with(vfs: Arc<dyn Vfs>, path: impl AsRef<Path>, options: Options) -> Result<Self> {
        let max_group_bytes = options.engine.max_group_bytes;
        let engine = Db::open_with(vfs, path, options.engine)?;
        match engine.get(keys::FORMAT)? {
            None => {
                engine.put(Bytes::from_static(keys::FORMAT), vec![FORMAT_VERSION])?;
            }
            Some(format) if format.as_ref() == [FORMAT_VERSION] => {}
            Some(format) => {
                return Err(invalid(format!(
                    "unsupported data layout {format:?}; this build reads layout {FORMAT_VERSION}"
                )))
            }
        }
        let catalog = Catalog::load(&engine)?;
        let ledger = Ledger::open(&engine, options.ledger, max_group_bytes)?;
        let db = Self {
            engine,
            catalog: RwLock::new(catalog),
            ledger,
            admin: Mutex::new(()),
            vocabularies: Vocabularies::default(),
            vector_indexes: VectorIndexes::default(),
        };
        db.finish_maintenance()?;
        if db.ledger.is_some() && options.verify_ledger != LedgerVerifyMode::None {
            let report = Ledger::verify(&db.engine, options.verify_ledger)?;
            if !report.is_valid {
                return Err(corruption(format!(
                    "ledger verification failed: {}",
                    report.error.unwrap_or_default()
                )));
            }
        }
        Ok(db)
    }

    /// Completes the index builds and drops an earlier process left unfinished.
    fn finish_maintenance(&self) -> Result<()> {
        let pending: Vec<(String, Vec<u64>, Vec<u64>)> = self
            .catalog
            .read()
            .schemas()
            .map(|(name, schema)| {
                (
                    name.clone(),
                    schema
                        .indexes
                        .iter()
                        .filter(|i| !i.ready)
                        .map(|i| i.id)
                        .collect::<Vec<_>>(),
                    schema.dropping.clone(),
                )
            })
            .filter(|(_, building, dropping)| !building.is_empty() || !dropping.is_empty())
            .collect();
        for (space, building, dropping) in pending {
            for id in building {
                self.build_index(&space, id)?;
            }
            for id in dropping {
                self.clear_index(&space, id)?;
            }
        }
        Ok(())
    }

    /// A session whose reads and writes are subject to every policy and masking rule.
    #[must_use]
    pub fn session(&self, context: SecurityContext) -> Session<'_> {
        Session::new(self, Access::Caller(context))
    }

    /// A session of trusted code: no policy or masking rule applies.
    #[must_use]
    pub fn system(&self) -> Session<'_> {
        Session::new(self, Access::System)
    }

    pub(crate) fn vector_indexes(&self) -> &VectorIndexes {
        &self.vector_indexes
    }

    /// The catalog, for a change: writers wait until it is released.
    pub(crate) fn catalog_mut(&self) -> parking_lot::RwLockWriteGuard<'_, Catalog> {
        self.catalog.write()
    }

    pub(crate) fn vocabularies(&self) -> &Vocabularies {
        &self.vocabularies
    }

    pub(crate) fn engine(&self) -> &Db {
        &self.engine
    }

    pub(crate) fn schema(&self, name: &str) -> Option<Arc<Schema>> {
        self.catalog.read().schema(name)
    }

    /// Plans with `plan` and commits, planning again while the plan goes stale.
    pub(crate) fn run(&self, plan: impl Fn(&Catalog) -> Result<Plan>) -> Result<Vec<Outcome>> {
        loop {
            let catalog = self.catalog.read();
            let mut plan = plan(&catalog)?;
            let vocabulary = std::mem::take(&mut plan.vocabulary);
            let vectors = std::mem::take(&mut plan.vectors);
            let attempt = match &self.ledger {
                Some(ledger) => ledger.commit(&self.engine, plan)?,
                None => self.commit_plain(&plan)?,
            };
            drop(catalog);
            if let Attempt::Committed { first, outcomes } = attempt {
                self.vocabularies.apply(&vocabulary, first);
                self.vector_indexes.apply(&vectors, first)?;
                return Ok(outcomes);
            }
        }
    }

    fn commit_plain(&self, plan: &Plan) -> Result<Attempt> {
        match self.engine.write(plan.batch()) {
            Ok(first) => Ok(Attempt::Committed {
                first,
                outcomes: plan.outcomes(first),
            }),
            Err(lilia_engine::Error::Conflict {
                key,
                expected,
                actual,
            }) => plan
                .conflict(&key, expected, actual)
                .map_or(Ok(Attempt::Retry), Err),
            Err(error) => Err(error.into()),
        }
    }

    /// Commits `writes` for `access`; `reads` are a transaction's reads, which must still hold.
    pub(crate) fn execute(
        &self,
        access: &Access,
        writes: &[Write],
        reads: Option<&HashMap<Bytes, Option<u64>>>,
    ) -> Result<Vec<Outcome>> {
        self.run(|catalog| {
            let mut planner = Planner::new(&self.engine, catalog, access, now_ms());
            for write in writes {
                planner.add(write)?;
            }
            if let Some(reads) = reads {
                planner.require(reads)?;
            }
            Ok(planner.finish())
        })
    }

    pub(crate) fn store_schema(
        &self,
        catalog: &mut Catalog,
        name: &str,
        schema: Schema,
    ) -> Result<()> {
        if schema.is_empty() {
            self.engine.delete(keys::schema(name))?;
        } else {
            self.engine.put(keys::schema(name), schema.encode()?)?;
        }
        catalog.set(name, schema);
        Ok(())
    }

    // ---- maintenance ----

    /// Deletes every expired key-value record (reads already skip them) and returns how many.
    /// With a ledger, each deletion is a ledger delta.
    ///
    /// # Errors
    ///
    /// Engine errors.
    pub fn purge_expired(&self) -> Result<u64> {
        let now = now_ms();
        let mut purged = 0u64;
        let mut chunk: Vec<(String, Vec<u8>)> = Vec::with_capacity(MAINTENANCE_CHUNK);
        let snapshot = self.engine.snapshot();
        let tags = [keys::KV, keys::KV + 1];
        for entry in self.engine.range_at(
            &snapshot,
            Bound::Included(&tags[..1]),
            Bound::Excluded(&tags[1..]),
        )? {
            let entry = entry?;
            if !decode_kv(&entry.value)?.expired(now) {
                continue;
            }
            let rest = &entry.key[1..];
            let split = rest
                .iter()
                .position(|&b| b == 0)
                .ok_or_else(|| corruption("key-value record key without a namespace"))?;
            let namespace = String::from_utf8(rest[..split].to_vec())
                .map_err(|_| corruption("namespace is not UTF-8"))?;
            chunk.push((namespace, rest[split + 1..].to_vec()));
            if chunk.len() == MAINTENANCE_CHUNK {
                purged += self.purge_chunk(&chunk)?;
                chunk.clear();
            }
        }
        if !chunk.is_empty() {
            purged += self.purge_chunk(&chunk)?;
        }
        Ok(purged)
    }

    fn purge_chunk(&self, chunk: &[(String, Vec<u8>)]) -> Result<u64> {
        let outcomes = self.run(|catalog| {
            let mut planner = Planner::new(&self.engine, catalog, &Access::System, now_ms());
            for (namespace, key) in chunk {
                planner.purge(namespace, key)?;
            }
            Ok(planner.finish())
        })?;
        Ok(outcomes.len() as u64)
    }

    /// Writes the memtable to a sorted run.
    ///
    /// # Errors
    ///
    /// Engine errors.
    pub fn flush(&self) -> Result<()> {
        Ok(self.engine.flush()?)
    }

    /// Closes the database, making every acknowledged write durable.
    ///
    /// # Errors
    ///
    /// The engine's failure, if writes had been disabled, or the final sync's.
    pub fn close(self) -> Result<()> {
        Ok(self.engine.close()?)
    }
}
