use std::collections::BTreeSet;

use serde_json::Value;

use crate::storage::{now_ms, validate_name};
use crate::Database;
use lilia_core::crdt::{LwwJsonDocument, OrSet, PNCounter};
use lilia_core::{
    compress_bytes, decompress_bytes, BatchOperation, CollectionKind, ErrorCode, LiliaError,
    Result, StorageTier,
};

/// Fluent interface for interacting with native CRDT data structures in storage.
#[derive(Debug, Clone, Copy)]
pub struct CrdtStore<'a> {
    pub(crate) db: &'a Database,
}

impl<'a> CrdtStore<'a> {
    /// Returns a store handle for a distributed PN-Counter.
    #[must_use]
    pub fn counter(&self, space: impl Into<String>, id: impl Into<String>) -> CounterStore<'a> {
        CounterStore {
            db: self.db,
            space: space.into(),
            id: id.into(),
        }
    }

    /// Returns a store handle for an Observed-Remove Set (OR-Set).
    #[must_use]
    pub fn set(&self, space: impl Into<String>, id: impl Into<String>) -> SetStore<'a> {
        SetStore {
            db: self.db,
            space: space.into(),
            id: id.into(),
        }
    }

    /// Returns a store handle for an LWW JSON document.
    #[must_use]
    pub fn doc(&self, space: impl Into<String>, id: impl Into<String>) -> DocStore<'a> {
        DocStore {
            db: self.db,
            space: space.into(),
            id: id.into(),
        }
    }
}

/// Store handle for a distributed Positive-Negative Counter.
#[derive(Debug)]
pub struct CounterStore<'a> {
    db: &'a Database,
    space: String,
    id: String,
}

impl CounterStore<'_> {
    /// Returns the current aggregated integer value of the counter (`sum(P) - sum(N)`).
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn get(&self) -> Result<i64> {
        validate_name("space", &self.space)?;
        validate_name("id", &self.id)?;
        match self.db.catalog.tier_for(CollectionKind::Crdt, &self.space) {
            StorageTier::Memory => {
                return Ok(self
                    .db
                    .mem_engine
                    .crdt_counters
                    .get(&self.space, self.id.as_bytes(), now_ms(), false)
                    .map_or(0, |snap| snap.value.value()));
            }
            StorageTier::Buffered => {
                self.db.ring_core.flush()?;
            }
            StorageTier::Disk => {}
        }
        let connection = self.db.read_lock()?;
        let mut stmt = connection.prepare(
            "SELECT COALESCE(SUM(p_count) - SUM(n_count), 0) FROM _lilia_crdt_counters WHERE space = ?1 AND id = ?2;",
        )?;
        stmt.bind_text_static(1, &self.space)?;
        stmt.bind_text_static(2, &self.id)?;
        if stmt.step()? {
            Ok(stmt.column_int64(0))
        } else {
            Ok(0)
        }
    }

    /// Retrieves the full distributed `PNCounter` state map across all actors.
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn state(&self) -> Result<PNCounter> {
        validate_name("space", &self.space)?;
        validate_name("id", &self.id)?;
        match self.db.catalog.tier_for(CollectionKind::Crdt, &self.space) {
            StorageTier::Memory => {
                return Ok(self
                    .db
                    .mem_engine
                    .crdt_counters
                    .get(&self.space, self.id.as_bytes(), now_ms(), false)
                    .map_or_else(PNCounter::new, |snap| snap.value));
            }
            StorageTier::Buffered => {
                self.db.ring_core.flush()?;
            }
            StorageTier::Disk => {}
        }
        let connection = self.db.read_lock()?;
        let mut stmt = connection.prepare(
            "SELECT actor_id, p_count, n_count FROM _lilia_crdt_counters WHERE space = ?1 AND id = ?2;",
        )?;
        stmt.bind_text_static(1, &self.space)?;
        stmt.bind_text_static(2, &self.id)?;
        let mut counter = PNCounter::new();
        while stmt.step()? {
            let actor = stmt.column_text(0);
            let p = stmt.column_int64(1);
            let n = stmt.column_int64(2);
            if p > 0 {
                counter.p.insert(actor.to_owned(), p.cast_unsigned());
            }
            if n > 0 {
                counter.n.insert(actor.to_owned(), n.cast_unsigned());
            }
        }
        Ok(counter)
    }

    /// Increments the counter by `amount` for `actor_id` and returns the new value.
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn increment(&self, actor_id: &str, amount: u64) -> Result<i64> {
        self.db.batch(&[BatchOperation::CrdtPnCounterIncrement {
            space: self.space.clone(),
            id: self.id.clone(),
            actor_id: actor_id.to_owned(),
            amount,
        }])?;
        self.get()
    }

    /// Decrements the counter by `amount` for `actor_id` and returns the new value.
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn decrement(&self, actor_id: &str, amount: u64) -> Result<i64> {
        self.db.batch(&[BatchOperation::CrdtPnCounterDecrement {
            space: self.space.clone(),
            id: self.id.clone(),
            actor_id: actor_id.to_owned(),
            amount,
        }])?;
        self.get()
    }

    /// Merges an incoming remote `PNCounter` state using the semilattice join rule.
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn merge(&self, remote: &PNCounter) -> Result<i64> {
        validate_name("space", &self.space)?;
        validate_name("id", &self.id)?;
        match self.db.catalog.tier_for(CollectionKind::Crdt, &self.space) {
            StorageTier::Memory => {
                return Ok(self
                    .db
                    .mem_engine
                    .crdt_counter_merge(&self.space, &self.id, remote));
            }
            StorageTier::Buffered => {
                self.db.ring_core.flush()?;
            }
            StorageTier::Disk => {}
        }
        let mut writer = self.db.lock_writer()?;
        let tx = writer.transaction()?;
        let mut stmt = tx.prepare(
            "INSERT INTO _lilia_crdt_counters(space, id, actor_id, p_count, n_count) \
             VALUES(?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(space, id, actor_id) DO UPDATE SET \
             p_count = MAX(_lilia_crdt_counters.p_count, excluded.p_count), \
             n_count = MAX(_lilia_crdt_counters.n_count, excluded.n_count);",
        )?;

        let mut actors = BTreeSet::new();
        actors.extend(remote.p.keys().cloned());
        actors.extend(remote.n.keys().cloned());

        for actor in actors {
            let p = remote.p.get(&actor).copied().unwrap_or(0);
            let n = remote.n.get(&actor).copied().unwrap_or(0);
            stmt.reset()?;
            stmt.bind_text_static(1, &self.space)?;
            stmt.bind_text_static(2, &self.id)?;
            stmt.bind_text_static(3, &actor)?;
            stmt.bind_int64(4, i64::try_from(p).unwrap_or(i64::MAX))?;
            stmt.bind_int64(5, i64::try_from(n).unwrap_or(i64::MAX))?;
            stmt.step()?;
        }
        drop(stmt);
        tx.commit()?;
        self.get()
    }
}

/// Store handle for an Observed-Remove Set (OR-Set).
#[derive(Debug)]
pub struct SetStore<'a> {
    db: &'a Database,
    space: String,
    id: String,
}

impl SetStore<'_> {
    /// Returns the set of all elements currently present (not tombstoned).
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn read(&self) -> Result<BTreeSet<String>> {
        validate_name("space", &self.space)?;
        validate_name("id", &self.id)?;
        match self.db.catalog.tier_for(CollectionKind::Crdt, &self.space) {
            StorageTier::Memory => {
                return Ok(self
                    .db
                    .mem_engine
                    .crdt_orset_elements(&self.space, &self.id));
            }
            StorageTier::Buffered => {
                self.db.ring_core.flush()?;
            }
            StorageTier::Disk => {}
        }
        let connection = self.db.read_lock()?;
        let mut stmt = connection.prepare(
            "SELECT DISTINCT element FROM _lilia_crdt_orsets WHERE space = ?1 AND id = ?2 AND tombstone_ms IS NULL ORDER BY element;",
        )?;
        stmt.bind_text_static(1, &self.space)?;
        stmt.bind_text_static(2, &self.id)?;
        let mut elements = BTreeSet::new();
        while stmt.step()? {
            elements.insert(stmt.column_text(0).to_owned());
        }
        Ok(elements)
    }

    /// Checks if a specific element is present in the set.
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn contains(&self, element: &str) -> Result<bool> {
        validate_name("space", &self.space)?;
        validate_name("id", &self.id)?;
        match self.db.catalog.tier_for(CollectionKind::Crdt, &self.space) {
            StorageTier::Memory => {
                return Ok(self
                    .db
                    .mem_engine
                    .crdt_orsets
                    .get(&self.space, self.id.as_bytes(), now_ms(), false)
                    .is_some_and(|snap| snap.value.contains(&element.to_owned())));
            }
            StorageTier::Buffered => {
                self.db.ring_core.flush()?;
            }
            StorageTier::Disk => {}
        }
        let connection = self.db.read_lock()?;
        let mut stmt = connection.prepare(
            "SELECT 1 FROM _lilia_crdt_orsets WHERE space = ?1 AND id = ?2 AND element = ?3 AND tombstone_ms IS NULL LIMIT 1;",
        )?;
        stmt.bind_text_static(1, &self.space)?;
        stmt.bind_text_static(2, &self.id)?;
        stmt.bind_text_static(3, element)?;
        let exists = stmt.step()?;
        Ok(exists)
    }

    /// Retrieves the complete in-memory `OrSet<String>` state.
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn state(&self) -> Result<OrSet<String>> {
        validate_name("space", &self.space)?;
        validate_name("id", &self.id)?;
        match self.db.catalog.tier_for(CollectionKind::Crdt, &self.space) {
            StorageTier::Memory => {
                return Ok(self
                    .db
                    .mem_engine
                    .crdt_orsets
                    .get(&self.space, self.id.as_bytes(), now_ms(), false)
                    .map_or_else(OrSet::new, |snap| snap.value));
            }
            StorageTier::Buffered => {
                self.db.ring_core.flush()?;
            }
            StorageTier::Disk => {}
        }
        let connection = self.db.read_lock()?;
        let mut stmt = connection.prepare(
            "SELECT element, tag, tombstone_ms FROM _lilia_crdt_orsets WHERE space = ?1 AND id = ?2;",
        )?;
        stmt.bind_text_static(1, &self.space)?;
        stmt.bind_text_static(2, &self.id)?;
        let mut set = OrSet::new();
        while stmt.step()? {
            let elem = stmt.column_text(0).to_owned();
            let tag = stmt.column_text(1).to_owned();
            let tombstone = if stmt.column_is_null(2) {
                None
            } else {
                Some(stmt.column_int64(2))
            };

            set.entries.entry(elem).or_default().insert(tag.clone());
            if let Some(ts) = tombstone {
                set.tombstones.insert(tag, ts);
            }
        }
        Ok(set)
    }

    /// Adds an element to the set on behalf of `actor_id`.
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn add(&self, actor_id: &str, element: &str) -> Result<()> {
        self.db.batch(&[BatchOperation::CrdtOrSetAdd {
            space: self.space.clone(),
            id: self.id.clone(),
            actor_id: actor_id.to_owned(),
            element: element.to_owned(),
        }])?;
        Ok(())
    }

    /// Removes an element from the set (tombstoning all currently observed tags).
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn remove(&self, element: &str) -> Result<()> {
        self.db.batch(&[BatchOperation::CrdtOrSetRemove {
            space: self.space.clone(),
            id: self.id.clone(),
            element: element.to_owned(),
            timestamp_ms: None,
        }])?;
        Ok(())
    }

    /// Prunes tombstones that are older than `cutoff_ms`.
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn prune(&self, cutoff_ms: i64) -> Result<usize> {
        validate_name("space", &self.space)?;
        validate_name("id", &self.id)?;
        match self.db.catalog.tier_for(CollectionKind::Crdt, &self.space) {
            StorageTier::Memory => {
                return Ok(self
                    .db
                    .mem_engine
                    .crdt_orset_prune(&self.space, &self.id, cutoff_ms));
            }
            StorageTier::Buffered => {
                self.db.ring_core.flush()?;
            }
            StorageTier::Disk => {}
        }
        let mut writer = self.db.lock_writer()?;
        let tx = writer.transaction()?;
        let mut stmt = tx.prepare(
            "DELETE FROM _lilia_crdt_orsets WHERE space = ?1 AND id = ?2 AND tombstone_ms IS NOT NULL AND tombstone_ms < ?3;",
        )?;
        stmt.bind_text_static(1, &self.space)?;
        stmt.bind_text_static(2, &self.id)?;
        stmt.bind_int64(3, cutoff_ms)?;
        stmt.step()?;
        drop(stmt);
        let changes = tx.conn().changes().cast_unsigned();
        tx.commit()?;
        Ok(usize::try_from(changes).unwrap_or(0))
    }

    /// Merges an incoming remote `OrSet<String>` state.
    ///
    /// # Errors
    /// Propagates errors from the operations it performs.
    pub fn merge(&self, remote: &OrSet<String>) -> Result<()> {
        validate_name("space", &self.space)?;
        validate_name("id", &self.id)?;
        match self.db.catalog.tier_for(CollectionKind::Crdt, &self.space) {
            StorageTier::Memory => {
                self.db
                    .mem_engine
                    .crdt_orset_merge(&self.space, &self.id, remote);
                return Ok(());
            }
            StorageTier::Buffered => {
                self.db.ring_core.flush()?;
            }
            StorageTier::Disk => {}
        }
        let mut writer = self.db.lock_writer()?;
        let tx = writer.transaction()?;
        let mut stmt = tx.prepare(
            "INSERT INTO _lilia_crdt_orsets(space, id, element, tag, tombstone_ms) \
             VALUES(?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(space, id, element, tag) DO UPDATE SET \
             tombstone_ms = COALESCE(_lilia_crdt_orsets.tombstone_ms, excluded.tombstone_ms);",
        )?;

        for (elem, tags) in &remote.entries {
            for tag in tags {
                let tombstone = remote.tombstones.get(tag).copied();
                stmt.reset()?;
                stmt.bind_text_static(1, &self.space)?;
                stmt.bind_text_static(2, &self.id)?;
                stmt.bind_text_static(3, elem)?;
                stmt.bind_text_static(4, tag)?;
                match tombstone {
                    Some(ts) => stmt.bind_int64(5, ts)?,
                    None => stmt.bind_null(5)?,
                }
                stmt.step()?;
            }
        }
        drop(stmt);
        tx.commit()?;
        Ok(())
    }
}

/// Store handle for an LWW JSON Document.
#[derive(Debug)]
pub struct DocStore<'a> {
    db: &'a Database,
    space: String,
    id: String,
}

impl DocStore<'_> {
    /// Retrieves the materialized JSON document.
    ///
    /// # Errors
    /// Returns `Storage` errors detected here, and propagates errors from the
    /// operations it performs.
    pub fn get(&self) -> Result<Option<Value>> {
        validate_name("space", &self.space)?;
        validate_name("id", &self.id)?;
        match self.db.catalog.tier_for(CollectionKind::Crdt, &self.space) {
            StorageTier::Memory => {
                return Ok(self
                    .db
                    .mem_engine
                    .crdt_json
                    .get(&self.space, self.id.as_bytes(), now_ms(), false)
                    .map(|snap| snap.value.1));
            }
            StorageTier::Buffered => {
                self.db.ring_core.flush()?;
            }
            StorageTier::Disk => {}
        }
        let connection = self.db.read_lock()?;
        let mut stmt = connection
            .prepare("SELECT materialized FROM _lilia_crdt_json WHERE space = ?1 AND id = ?2;")?;
        stmt.bind_text_static(1, &self.space)?;
        stmt.bind_text_static(2, &self.id)?;
        if stmt.step()? {
            let json_str = stmt.column_text(0);
            let val = serde_json::from_str(json_str)
                .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
            Ok(Some(val))
        } else {
            Ok(None)
        }
    }

    /// Retrieves the internal `LwwJsonDocument` state tracking all field clocks.
    ///
    /// # Errors
    /// Returns `Storage` errors detected here, and propagates errors from the
    /// operations it performs.
    pub fn state(&self) -> Result<Option<LwwJsonDocument>> {
        validate_name("space", &self.space)?;
        validate_name("id", &self.id)?;
        match self.db.catalog.tier_for(CollectionKind::Crdt, &self.space) {
            StorageTier::Memory => {
                return Ok(self
                    .db
                    .mem_engine
                    .crdt_json
                    .get(&self.space, self.id.as_bytes(), now_ms(), false)
                    .map(|snap| snap.value.0));
            }
            StorageTier::Buffered => {
                self.db.ring_core.flush()?;
            }
            StorageTier::Disk => {}
        }
        let connection = self.db.read_lock()?;
        let mut stmt = connection
            .prepare("SELECT state FROM _lilia_crdt_json WHERE space = ?1 AND id = ?2;")?;
        stmt.bind_text_static(1, &self.space)?;
        stmt.bind_text_static(2, &self.id)?;
        if stmt.step()? {
            let blob = stmt.column_blob(0);
            let raw = decompress_bytes(blob)?;
            let doc: LwwJsonDocument = serde_json::from_slice(&raw)
                .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
            Ok(Some(doc))
        } else {
            Ok(None)
        }
    }

    /// Sets an individual field path in the document.
    ///
    /// # Errors
    /// Returns `Storage` errors detected here, and propagates errors from the
    /// operations it performs.
    pub fn set_field(
        &self,
        actor_id: &str,
        path: &str,
        value: Value,
        timestamp_ms: Option<i64>,
    ) -> Result<u64> {
        let results = self.db.batch(&[BatchOperation::CrdtJsonSetField {
            space: self.space.clone(),
            id: self.id.clone(),
            actor_id: actor_id.to_owned(),
            path: path.to_owned(),
            value,
            timestamp_ms,
        }])?;
        results
            .first()
            .and_then(|r| {
                if r.buffered {
                    Some(r.version.unwrap_or(0))
                } else {
                    r.version
                }
            })
            .ok_or_else(|| {
                LiliaError::new(ErrorCode::Storage, "missing version after crdt put", false)
            })
    }

    /// Marks an individual field path (and its children) as deleted.
    ///
    /// # Errors
    /// Returns `Storage` errors detected here, and propagates errors from the
    /// operations it performs.
    pub fn delete_field(
        &self,
        actor_id: &str,
        path: &str,
        timestamp_ms: Option<i64>,
    ) -> Result<u64> {
        let results = self.db.batch(&[BatchOperation::CrdtJsonDeleteField {
            space: self.space.clone(),
            id: self.id.clone(),
            actor_id: actor_id.to_owned(),
            path: path.to_owned(),
            timestamp_ms,
        }])?;
        results
            .first()
            .and_then(|r| {
                if r.buffered {
                    Some(r.version.unwrap_or(0))
                } else {
                    r.version
                }
            })
            .ok_or_else(|| {
                LiliaError::new(
                    ErrorCode::Storage,
                    "missing version after crdt delete",
                    false,
                )
            })
    }

    /// Merges a complete remote `LwwJsonDocument` state into this document.
    ///
    /// # Errors
    /// Returns `Storage` errors detected here, and propagates errors from the
    /// operations it performs.
    pub fn merge(&self, remote: &LwwJsonDocument) -> Result<u64> {
        let raw = serde_json::to_vec(remote)
            .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
        let state = compress_bytes(&raw);
        let results = self.db.batch(&[BatchOperation::CrdtJsonMerge {
            space: self.space.clone(),
            id: self.id.clone(),
            state,
        }])?;
        results
            .first()
            .and_then(|r| {
                if r.buffered {
                    Some(r.version.unwrap_or(0))
                } else {
                    r.version
                }
            })
            .ok_or_else(|| {
                LiliaError::new(
                    ErrorCode::Storage,
                    "missing version after crdt merge",
                    false,
                )
            })
    }
}

impl Database {
    /// Fluent interface for interacting with native CRDT data structures in storage.
    #[must_use]
    pub fn crdt(&self) -> CrdtStore<'_> {
        CrdtStore { db: self }
    }
}
