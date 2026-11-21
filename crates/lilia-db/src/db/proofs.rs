//! The ledger: its state, commit and record proofs, verification.

use lilia_core::{LedgerCommit, LedgerVerificationReport, LedgerVerifyMode};

use super::Database;
use crate::error::{Error, Result};
use crate::keys::{self, validate_name};
use crate::ledger::{CommitProof, Ledger, RecordProof};
use crate::read::Latest;

impl Database {
    fn ledger(&self) -> Result<&Ledger> {
        self.ledger.as_ref().ok_or(Error::LedgerDisabled)
    }

    /// The ledger's current root, which attests every commit so far.
    ///
    /// # Errors
    ///
    /// [`Error::LedgerDisabled`]; engine errors.
    pub fn ledger_root(&self) -> Result<[u8; 32]> {
        self.ledger()?;
        Ok(Ledger::state(&self.engine)?.0)
    }

    /// Number of ledger commits.
    ///
    /// # Errors
    ///
    /// [`Error::LedgerDisabled`]; engine errors.
    pub fn ledger_len(&self) -> Result<u64> {
        self.ledger()?;
        Ok(Ledger::state(&self.engine)?.1)
    }

    /// The header of ledger commit `sequence` (1-based).
    ///
    /// # Errors
    ///
    /// [`Error::LedgerDisabled`]; [`Error::NotFound`] past the last commit.
    pub fn ledger_commit(&self, sequence: u64) -> Result<LedgerCommit> {
        self.ledger()?;
        Ledger::commit_header(&self.engine, sequence)
    }

    /// Proof that commit `sequence` (1-based) is in the ledger.
    ///
    /// # Errors
    ///
    /// As [`Database::ledger_commit`].
    pub fn prove_commit(&self, sequence: u64) -> Result<CommitProof> {
        self.ledger()?;
        Ledger::prove_commit(&self.engine, sequence)
    }

    /// Proof that the current version of key `key` of `namespace` was written by a ledger commit.
    ///
    /// # Errors
    ///
    /// [`Error::LedgerDisabled`]; [`Error::NotFound`] for an absent record or one older than
    /// the ledger.
    pub fn prove_kv(&self, namespace: &str, key: &[u8]) -> Result<RecordProof> {
        self.ledger()?;
        validate_name("namespace", namespace)?;
        Ledger::prove_record(&self.engine, &keys::kv(namespace, key))
    }

    /// Proof that the current version of document `id` of `space` was written by a ledger commit.
    ///
    /// # Errors
    ///
    /// As [`Database::prove_kv`].
    pub fn prove_doc(&self, space: &str, id: &str) -> Result<RecordProof> {
        self.ledger()?;
        validate_name("space", space)?;
        Ledger::prove_record(&self.engine, &keys::doc(space, id))
    }

    /// Proof that the current version of node `id` of graph `graph` was written by a ledger
    /// commit.
    ///
    /// # Errors
    ///
    /// As [`Database::prove_kv`].
    pub fn prove_node(&self, graph: &str, id: &str) -> Result<RecordProof> {
        self.ledger()?;
        validate_name("graph", graph)?;
        Ledger::prove_record(&self.engine, &crate::graph::node_key(graph, id))
    }

    /// Proof that the current version of the edge `source -kind-> target` of graph `graph` was
    /// written by a ledger commit.
    ///
    /// # Errors
    ///
    /// As [`Database::prove_kv`].
    pub fn prove_edge(
        &self,
        graph: &str,
        source: &str,
        target: &str,
        kind: &str,
    ) -> Result<RecordProof> {
        self.ledger()?;
        validate_name("graph", graph)?;
        let key = crate::graph::edge_key(keys::EDGE_OUT, graph, source, kind, target);
        Ledger::prove_record(&self.engine, &key)
    }

    /// Proof that the current version of document `doc_id` of full-text collection
    /// `collection` was written by a ledger commit.
    ///
    /// # Errors
    ///
    /// As [`Database::prove_kv`].
    pub fn prove_fts_doc(&self, collection: &str, doc_id: &str) -> Result<RecordProof> {
        self.ledger()?;
        validate_name("collection", collection)?;
        Ledger::prove_record(&self.engine, &crate::fts::doc_key(collection, doc_id))
    }

    /// Proof that the current version of vector `id` of collection `collection` was written by
    /// a ledger commit.
    ///
    /// # Errors
    ///
    /// As [`Database::prove_kv`].
    pub fn prove_vector(&self, collection: &str, id: &str) -> Result<RecordProof> {
        self.ledger()?;
        validate_name("collection", collection)?;
        Ledger::prove_record(self.engine(), &crate::vectors::vector_key(collection, id))
    }

    /// Proof that the current version of fence `id` of geo collection `collection` was written
    /// by a ledger commit.
    ///
    /// # Errors
    ///
    /// As [`Database::prove_kv`].
    pub fn prove_fence(&self, collection: &str, id: &str) -> Result<RecordProof> {
        self.ledger()?;
        validate_name("collection", collection)?;
        Ledger::prove_record(self.engine(), &crate::geo::fence_key(collection, id))
    }

    /// Proof that the current version of the point of `track` at `timestamp` in geo collection
    /// `collection` was written by a ledger commit.
    ///
    /// # Errors
    ///
    /// As [`Database::prove_kv`].
    pub fn prove_gps_point(
        &self,
        collection: &str,
        track: &str,
        timestamp: i64,
    ) -> Result<RecordProof> {
        self.ledger()?;
        validate_name("collection", collection)?;
        validate_name("track", track)?;
        Ledger::prove_record(
            self.engine(),
            &crate::geo::point_key(collection, track, timestamp),
        )
    }

    /// Proof that the current slice of entity `id` of bitemporal space `space` starting at valid
    /// time `vt_from` was written by a ledger commit.
    ///
    /// # Errors
    ///
    /// As [`Database::prove_kv`].
    pub fn prove_bitemporal(&self, space: &str, id: &str, vt_from: i64) -> Result<RecordProof> {
        self.ledger()?;
        validate_name("space", space)?;
        validate_name("entity id", id)?;
        Ledger::prove_record(
            self.engine(),
            &crate::bitemporal::current_key(space, id, vt_from),
        )
    }

    /// Proof that the current totals of `actor` in PN-counter `id` of CRDT space `space` were
    /// written by a ledger commit.
    ///
    /// # Errors
    ///
    /// As [`Database::prove_kv`].
    pub fn prove_crdt_counter(&self, space: &str, id: &str, actor: &str) -> Result<RecordProof> {
        self.ledger()?;
        validate_name("space", space)?;
        validate_name("CRDT id", id)?;
        Ledger::prove_record(self.engine(), &crate::crdt::counter_key(space, id, actor))
    }

    /// Proof that the current state of the entry of `element` under `tag` in OR-set `id` of
    /// CRDT space `space` was written by a ledger commit.
    ///
    /// # Errors
    ///
    /// As [`Database::prove_kv`].
    pub fn prove_crdt_set_entry(
        &self,
        space: &str,
        id: &str,
        element: &str,
        tag: &str,
    ) -> Result<RecordProof> {
        self.ledger()?;
        validate_name("space", space)?;
        validate_name("CRDT id", id)?;
        Ledger::prove_record(
            self.engine(),
            &crate::crdt::entry_key(space, id, element, tag),
        )
    }

    /// Proof that the current version of field `path` of LWW document `id` of CRDT space `space`
    /// was written by a ledger commit.
    ///
    /// # Errors
    ///
    /// An invalid path; as [`Database::prove_kv`].
    pub fn prove_crdt_field(&self, space: &str, id: &str, path: &str) -> Result<RecordProof> {
        self.ledger()?;
        validate_name("space", space)?;
        validate_name("CRDT id", id)?;
        let path = crate::crdt::normalize_path(path)?;
        Ledger::prove_record(self.engine(), &crate::crdt::field_key(space, id, &path))
    }

    /// Proof that the current version of the chunk holding the point of series `series` at
    /// `timestamp`, resolution `resolution_ms`, was written by a ledger commit. The delta's id
    /// is the chunk's `resolution:bound`.
    ///
    /// # Errors
    ///
    /// As [`Database::prove_kv`]; [`Error::NotFound`] when there is no point at `timestamp`.
    pub fn prove_series(
        &self,
        series: &str,
        resolution_ms: i64,
        timestamp: i64,
    ) -> Result<RecordProof> {
        self.ledger()?;
        validate_name("series", series)?;
        crate::timeseries::validate_resolution(resolution_ms)?;
        let reader = Latest::pinned(self.engine());
        let key = crate::timeseries::chunk_holding(&reader, series, resolution_ms, timestamp)?
            .ok_or_else(|| {
                Error::NotFound(format!("series {series} has no point at {timestamp}"))
            })?;
        Ledger::prove_record(self.engine(), &key)
    }

    /// Verifies the ledger; see [`Ledger::verify`].
    ///
    /// # Errors
    ///
    /// [`Error::LedgerDisabled`]; engine errors. Integrity failures are in the report.
    pub fn verify_ledger(&self, mode: LedgerVerifyMode) -> Result<LedgerVerificationReport> {
        self.ledger()?;
        Ledger::verify(&self.engine, mode)
    }
}
