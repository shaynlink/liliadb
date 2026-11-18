//! Cryptographic Merkle Ledger Manager for `lilia-storage-sqlite`.
//!
//! Enforces atomic cryptographic commit chaining via BLAKE3, maintains the Merkle DAG
//! in `_lilia_ledger` and `_lilia_ledger_leaves`, generates mathematical inclusion proofs,
//! and performs tamper detection on database startup.

use std::fmt::Write as _;
use std::sync::RwLock;

use lilia_core::ledger::{
    compute_commit_hash, InclusionProof, LedgerAccumulator, LedgerAction, LedgerCommit,
    LedgerDeltaEntry, LedgerVerificationReport, LedgerVerifyMode, MerkleTree,
};
use lilia_core::{BatchOperation, ErrorCode, LiliaError, MutationResult, Result, StorageTier};
use lilia_sqlite_native::{NativeConnection, NativeStatement};

/// Rows written through the writer connection, queued by the capture triggers until the next
/// ledger commit records their final state.
const TOUCHED_TABLE: &str = "_lilia_ledger_touched";

/// Tables holding the commit chain: raw SQL may read them but never write them.
pub(crate) const LEDGER_TABLES: &[&str] = &["_lilia_ledger", "_lilia_ledger_leaves"];

#[derive(Debug)]
pub(crate) struct LedgerManager {
    enabled: bool,
    inner: RwLock<LedgerInner>,
}

#[derive(Debug, Default)]
struct LedgerInner {
    latest_commit_seq: u64,
    latest_commit_hash: [u8; 32],
    latest_merkle_root: [u8; 32],
    accumulator: LedgerAccumulator,
}

/// Key of a row queued in the touched table.
enum TouchedKey {
    Json(String),
    Kv(Vec<u8>),
}

struct Touched {
    space: String,
    key: TouchedKey,
    last_version: u64,
}

impl LedgerManager {
    /// Opens the ledger on the writer connection `conn`, verifies the database according to
    /// `verify_mode` and installs the capture triggers.
    ///
    /// A database without ledger history (created before the ledger, or while it was disabled)
    /// gets a genesis commit of its current documents and entries instead of a verification.
    pub(crate) fn open(
        conn: &NativeConnection,
        enabled: bool,
        verify_mode: LedgerVerifyMode,
    ) -> Result<Self> {
        if !enabled {
            return Ok(Self {
                enabled: false,
                inner: RwLock::new(LedgerInner::default()),
            });
        }

        let inner = load_chain(conn, verify_mode != LedgerVerifyMode::None)?;
        let has_history = inner.latest_commit_seq > 0;
        let manager = Self {
            enabled: true,
            inner: RwLock::new(inner),
        };
        if !has_history {
            manager.record_genesis(conn)?;
        } else if verify_mode == LedgerVerifyMode::Strict {
            Self::verify_live_state(conn)?;
        }
        conn.exec(&capture_schema())?;
        Ok(manager)
    }

    /// Commits every document and entry already stored, in one transaction.
    fn record_genesis(&self, conn: &NativeConnection) -> Result<()> {
        conn.exec("BEGIN IMMEDIATE;")?;
        let outcome = live_entries(conn)
            .and_then(|entries| {
                if entries.is_empty() {
                    Ok(())
                } else {
                    self.append_commit(conn, &entries, crate::storage::now_ms())
                }
            })
            .and_then(|()| conn.exec("COMMIT;"));
        if let Err(error) = outcome {
            if !conn.is_autocommit() {
                conn.exec("ROLLBACK;")?;
            }
            *self.write_inner()? = LedgerInner::default();
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    fn write_inner(&self) -> Result<std::sync::RwLockWriteGuard<'_, LedgerInner>> {
        self.inner
            .write()
            .map_err(|_| LiliaError::new(ErrorCode::Storage, "ledger lock poisoned", false))
    }

    /// Verifies that all live rows in `_lilia_json` and `_lilia_kv` of the main database (shards
    /// are not covered) match their cryptographic commitments.
    pub(crate) fn verify_live_state(conn: &NativeConnection) -> Result<u64> {
        let mut leaf = conn.prepare(
            "SELECT leaf_hash FROM _lilia_ledger_leaves \
             WHERE model = ?1 AND space = ?2 AND id = ?3 AND version = ?4;",
        )?;
        let mut count = 0u64;
        let mut json = conn.prepare("SELECT space, id, version, value FROM main._lilia_json;")?;
        while json.step()? {
            let entry = put_entry(
                "json",
                json.column_text(0),
                json.column_text(1).to_owned(),
                json.column_int64(2),
                json.column_blob(3),
            );
            verify_live_entry(&mut leaf, &entry, "document")?;
            count += 1;
        }
        let mut kv = conn.prepare("SELECT namespace, key, version, value FROM main._lilia_kv;")?;
        while kv.step()? {
            let entry = put_entry(
                "kv",
                kv.column_text(0),
                hex::encode(kv.column_blob(1)),
                kv.column_int64(2),
                kv.column_blob(3),
            );
            verify_live_entry(&mut leaf, &entry, "KV entry")?;
            count += 1;
        }
        Ok(count)
    }

    /// Appends one ledger commit covering every JSON and KV row the writer connection changed
    /// since the previous commit, plus `entries` for the models the capture triggers do not see.
    ///
    /// Call it inside the write transaction, right before it commits, with that transaction's
    /// connection: the ledger rows then commit or roll back with the data they prove.
    pub(crate) fn record_commit(
        &self,
        conn: &NativeConnection,
        entries: Vec<LedgerDeltaEntry>,
        now: i64,
    ) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let mut delta = touched_entries(conn)?;
        delta.extend(entries);
        if delta.is_empty() {
            return Ok(());
        }
        self.append_commit(conn, &delta, now)
    }

    fn append_commit(
        &self,
        conn: &NativeConnection,
        delta: &[LedgerDeltaEntry],
        now: i64,
    ) -> Result<()> {
        let leaf_hashes: Vec<[u8; 32]> = delta
            .iter()
            .map(LedgerDeltaEntry::compute_leaf_hash)
            .collect();
        let tx_root = MerkleTree::build(&leaf_hashes).root();
        let operation_count = u32::try_from(delta.len()).map_err(|_| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                "too many ledger entries in one commit",
                false,
            )
        })?;

        let mut inner = self.write_inner()?;
        // The in-memory chain runs ahead of the stored one when a transaction that recorded a
        // commit was rolled back afterwards: start again from what is stored.
        if stored_commit_seq(conn)? != inner.latest_commit_seq {
            *inner = load_chain(conn, false)?;
        }

        let commit_seq = inner.latest_commit_seq + 1;
        let parent_hash = inner.latest_commit_hash;
        let commit_hash =
            compute_commit_hash(commit_seq, now, &parent_hash, &tx_root, operation_count);
        let delta_payload = serde_json::to_vec(delta).map_err(|e| {
            LiliaError::new(
                ErrorCode::Storage,
                format!("failed to serialize delta: {e}"),
                false,
            )
        })?;
        let mut accumulator = std::mem::take(&mut inner.accumulator);
        let merkle_root = accumulator.append(commit_hash);

        let mut stmt_commit = conn.prepare(
            "INSERT INTO _lilia_ledger(commit_seq, timestamp_ms, parent_hash, tx_root, commit_hash, merkle_root, operation_count, delta_payload, metadata) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL);",
        )?;
        stmt_commit.bind_int64(1, stored_int(commit_seq))?;
        stmt_commit.bind_int64(2, now)?;
        stmt_commit.bind_blob_static(3, &parent_hash)?;
        stmt_commit.bind_blob_static(4, &tx_root)?;
        stmt_commit.bind_blob_static(5, &commit_hash)?;
        stmt_commit.bind_blob_static(6, &merkle_root)?;
        stmt_commit.bind_int64(7, i64::from(operation_count))?;
        stmt_commit.bind_blob_static(8, &delta_payload)?;
        let inserted = stmt_commit.step().and_then(|_| {
            let mut stmt_leaf = conn.prepare(
                "INSERT OR REPLACE INTO _lilia_ledger_leaves(model, space, id, version, commit_seq, leaf_index, leaf_hash) \
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7);",
            )?;
            for (index, (entry, leaf_hash)) in delta.iter().zip(&leaf_hashes).enumerate() {
                stmt_leaf.reset()?;
                stmt_leaf.bind_text_static(1, &entry.model)?;
                stmt_leaf.bind_text_static(2, &entry.space)?;
                stmt_leaf.bind_text_static(3, &entry.id)?;
                stmt_leaf.bind_int64(4, stored_int(entry.version))?;
                stmt_leaf.bind_int64(5, stored_int(commit_seq))?;
                stmt_leaf.bind_int64(6, stored_int(u64::try_from(index).unwrap_or(u64::MAX)))?;
                stmt_leaf.bind_blob_static(7, leaf_hash)?;
                stmt_leaf.step()?;
            }
            Ok(())
        });
        if let Err(error) = inserted {
            *inner = load_chain(conn, false)?;
            return Err(error);
        }

        inner.accumulator = accumulator;
        inner.latest_commit_seq = commit_seq;
        inner.latest_commit_hash = commit_hash;
        inner.latest_merkle_root = merkle_root;
        Ok(())
    }

    /// Returns the current Merkle root of the database.
    pub(crate) fn root(&self) -> [u8; 32] {
        self.inner
            .read()
            .map_or([0u8; 32], |i| i.latest_merkle_root)
    }

    /// Returns the latest commit header.
    pub(crate) fn latest_commit(&self, conn: &NativeConnection) -> Result<Option<LedgerCommit>> {
        let seq = self.inner.read().map_or(0, |i| i.latest_commit_seq);
        if seq == 0 {
            return Ok(None);
        }
        Self::commit_by_seq(conn, seq)
    }

    /// Returns a commit header by its 1-based sequence number.
    pub(crate) fn commit_by_seq(conn: &NativeConnection, seq: u64) -> Result<Option<LedgerCommit>> {
        let mut stmt = conn.prepare(
            "SELECT commit_seq, timestamp_ms, parent_hash, tx_root, commit_hash, merkle_root, operation_count, metadata \
             FROM _lilia_ledger WHERE commit_seq = ?1;",
        )?;
        stmt.bind_int64(1, stored_int(seq))?;
        if !stmt.step()? {
            return Ok(None);
        }

        let commit_seq = stored_version(stmt.column_int64(0));
        let timestamp_ms = stmt.column_int64(1);
        let parent_hash: [u8; 32] = stmt
            .column_blob(2)
            .try_into()
            .map_err(|_| LiliaError::new(ErrorCode::Storage, "invalid parent_hash blob", false))?;
        let tx_root: [u8; 32] = stmt
            .column_blob(3)
            .try_into()
            .map_err(|_| LiliaError::new(ErrorCode::Storage, "invalid tx_root blob", false))?;
        let commit_hash: [u8; 32] = stmt
            .column_blob(4)
            .try_into()
            .map_err(|_| LiliaError::new(ErrorCode::Storage, "invalid commit_hash blob", false))?;
        let merkle_root: [u8; 32] = stmt
            .column_blob(5)
            .try_into()
            .map_err(|_| LiliaError::new(ErrorCode::Storage, "invalid merkle_root blob", false))?;
        let operation_count = stored_count(stmt.column_int64(6));
        let metadata = if stmt.column_is_null(7) {
            None
        } else {
            Some(stmt.column_text(7).to_string())
        };

        Ok(Some(LedgerCommit {
            commit_seq,
            timestamp_ms,
            parent_hash,
            tx_root,
            commit_hash,
            merkle_root,
            operation_count,
            metadata,
        }))
    }

    /// Exports an inclusion proof for a JSON document.
    pub(crate) fn prove_doc(
        &self,
        conn: &NativeConnection,
        space: &str,
        id: &str,
        version: Option<u64>,
    ) -> Result<InclusionProof> {
        let (ver, commit_seq, leaf_idx, leaf_hash) = if let Some(v) = version {
            let mut stmt = conn.prepare(
                "SELECT version, commit_seq, leaf_index, leaf_hash FROM _lilia_ledger_leaves \
                 WHERE model = 'json' AND space = ?1 AND id = ?2 AND version = ?3;",
            )?;
            stmt.bind_text_static(1, space)?;
            stmt.bind_text_static(2, id)?;
            stmt.bind_int64(3, stored_int(v))?;
            if !stmt.step()? {
                return Err(LiliaError::new(
                    ErrorCode::NotFound,
                    format!("document 'json:{space}/{id}' (v{v}) not found in ledger"),
                    false,
                ));
            }
            let h: [u8; 32] = stmt.column_blob(3).try_into().map_err(|_| {
                LiliaError::new(ErrorCode::Storage, "invalid leaf_hash blob", false)
            })?;
            (
                v,
                stored_version(stmt.column_int64(1)),
                stored_index(stmt.column_int64(2)),
                h,
            )
        } else {
            let mut stmt = conn.prepare(
                "SELECT version, commit_seq, leaf_index, leaf_hash FROM _lilia_ledger_leaves \
                 WHERE model = 'json' AND space = ?1 AND id = ?2 ORDER BY version DESC LIMIT 1;",
            )?;
            stmt.bind_text_static(1, space)?;
            stmt.bind_text_static(2, id)?;
            if !stmt.step()? {
                return Err(LiliaError::new(
                    ErrorCode::NotFound,
                    format!("document 'json:{space}/{id}' not found in ledger"),
                    false,
                ));
            }
            let v = stored_version(stmt.column_int64(0));
            let h: [u8; 32] = stmt.column_blob(3).try_into().map_err(|_| {
                LiliaError::new(ErrorCode::Storage, "invalid leaf_hash blob", false)
            })?;
            (
                v,
                stored_version(stmt.column_int64(1)),
                stored_index(stmt.column_int64(2)),
                h,
            )
        };

        self.build_proof(
            conn,
            format!("json:{space}/{id}:v{ver}"),
            commit_seq,
            leaf_idx,
            leaf_hash,
        )
    }

    /// Exports an inclusion proof for a KV entry.
    pub(crate) fn prove_kv(
        &self,
        conn: &NativeConnection,
        namespace: &str,
        key: &[u8],
        version: Option<u64>,
    ) -> Result<InclusionProof> {
        let key_str = hex::encode(key);
        let (ver, commit_seq, leaf_idx, leaf_hash) = if let Some(v) = version {
            let mut stmt = conn.prepare(
                "SELECT version, commit_seq, leaf_index, leaf_hash FROM _lilia_ledger_leaves \
                 WHERE model = 'kv' AND space = ?1 AND id = ?2 AND version = ?3;",
            )?;
            stmt.bind_text_static(1, namespace)?;
            stmt.bind_text_static(2, &key_str)?;
            stmt.bind_int64(3, stored_int(v))?;
            if !stmt.step()? {
                return Err(LiliaError::new(
                    ErrorCode::NotFound,
                    format!("KV entry 'kv:{namespace}/{key_str}' (v{v}) not found in ledger"),
                    false,
                ));
            }
            let h: [u8; 32] = stmt.column_blob(3).try_into().map_err(|_| {
                LiliaError::new(ErrorCode::Storage, "invalid leaf_hash blob", false)
            })?;
            (
                v,
                stored_version(stmt.column_int64(1)),
                stored_index(stmt.column_int64(2)),
                h,
            )
        } else {
            let mut stmt = conn.prepare(
                "SELECT version, commit_seq, leaf_index, leaf_hash FROM _lilia_ledger_leaves \
                 WHERE model = 'kv' AND space = ?1 AND id = ?2 ORDER BY version DESC LIMIT 1;",
            )?;
            stmt.bind_text_static(1, namespace)?;
            stmt.bind_text_static(2, &key_str)?;
            if !stmt.step()? {
                return Err(LiliaError::new(
                    ErrorCode::NotFound,
                    format!("KV entry 'kv:{namespace}/{key_str}' not found in ledger"),
                    false,
                ));
            }
            let v = stored_version(stmt.column_int64(0));
            let h: [u8; 32] = stmt.column_blob(3).try_into().map_err(|_| {
                LiliaError::new(ErrorCode::Storage, "invalid leaf_hash blob", false)
            })?;
            (
                v,
                stored_version(stmt.column_int64(1)),
                stored_index(stmt.column_int64(2)),
                h,
            )
        };

        self.build_proof(
            conn,
            format!("kv:{namespace}/{key_str}:v{ver}"),
            commit_seq,
            leaf_idx,
            leaf_hash,
        )
    }

    /// Exports an inclusion proof for an entire commit.
    pub(crate) fn prove_commit(
        &self,
        conn: &NativeConnection,
        commit_seq: u64,
    ) -> Result<InclusionProof> {
        let commit = Self::commit_by_seq(conn, commit_seq)?.ok_or_else(|| {
            LiliaError::new(
                ErrorCode::NotFound,
                format!("commit sequence {commit_seq} not found in ledger"),
                false,
            )
        })?;

        let inner = self
            .inner
            .read()
            .map_err(|_| LiliaError::new(ErrorCode::Storage, "ledger lock poisoned", false))?;

        let ledger_audit_path = inner
            .accumulator
            .audit_path_for_commit(commit_seq)
            .ok_or_else(|| {
                LiliaError::new(
                    ErrorCode::NotFound,
                    format!("audit path for commit {commit_seq} unavailable"),
                    false,
                )
            })?;

        Ok(InclusionProof {
            target: format!("commit:{commit_seq}"),
            leaf_hash: commit.commit_hash,
            commit_seq,
            timestamp_ms: commit.timestamp_ms,
            tx_root: commit.tx_root,
            commit_hash: commit.commit_hash,
            tx_audit_path: Vec::new(),
            ledger_audit_path,
            merkle_root: inner.latest_merkle_root,
        })
    }

    fn build_proof(
        &self,
        conn: &NativeConnection,
        target: String,
        commit_seq: u64,
        leaf_idx: usize,
        leaf_hash: [u8; 32],
    ) -> Result<InclusionProof> {
        let mut stmt = conn.prepare(
            "SELECT timestamp_ms, tx_root, commit_hash, delta_payload FROM _lilia_ledger WHERE commit_seq = ?1;",
        )?;
        stmt.bind_int64(1, stored_int(commit_seq))?;
        if !stmt.step()? {
            return Err(LiliaError::new(
                ErrorCode::NotFound,
                format!("commit {commit_seq} not found for proof"),
                false,
            ));
        }

        let timestamp_ms = stmt.column_int64(0);
        let tx_root: [u8; 32] = stmt
            .column_blob(1)
            .try_into()
            .map_err(|_| LiliaError::new(ErrorCode::Storage, "invalid tx_root blob", false))?;
        let commit_hash: [u8; 32] = stmt
            .column_blob(2)
            .try_into()
            .map_err(|_| LiliaError::new(ErrorCode::Storage, "invalid commit_hash blob", false))?;
        let delta_payload = stmt.column_blob(3);

        let delta_entries: Vec<LedgerDeltaEntry> =
            serde_json::from_slice(delta_payload).map_err(|e| {
                LiliaError::new(
                    ErrorCode::Storage,
                    format!("invalid delta payload: {e}"),
                    false,
                )
            })?;
        let leaf_hashes: Vec<[u8; 32]> = delta_entries
            .iter()
            .map(LedgerDeltaEntry::compute_leaf_hash)
            .collect();
        let tx_tree = MerkleTree::build(&leaf_hashes);
        let tx_audit_path = tx_tree.audit_path(leaf_idx);

        let inner = self
            .inner
            .read()
            .map_err(|_| LiliaError::new(ErrorCode::Storage, "ledger lock poisoned", false))?;

        let ledger_audit_path = inner
            .accumulator
            .audit_path_for_commit(commit_seq)
            .ok_or_else(|| {
                LiliaError::new(
                    ErrorCode::NotFound,
                    format!("ledger audit path for commit {commit_seq} unavailable"),
                    false,
                )
            })?;

        Ok(InclusionProof {
            target,
            leaf_hash,
            commit_seq,
            timestamp_ms,
            tx_root,
            commit_hash,
            tx_audit_path,
            ledger_audit_path,
            merkle_root: inner.latest_merkle_root,
        })
    }

    /// Full ledger verification check returning a detailed report.
    #[allow(clippy::too_many_lines)] // one sequential procedure over shared state; splitting it would only thread that state through helpers
    pub(crate) fn verify_ledger(
        &self,
        conn: &NativeConnection,
    ) -> Result<LedgerVerificationReport> {
        let commit_count = self.inner.read().map_or(0, |i| i.latest_commit_seq);
        let root = self.root();

        // 1. Verify DAG
        let mut expected_seq = 1u64;
        let mut expected_parent = [0u8; 32];
        let mut commit_hashes = Vec::new();

        let mut stmt = conn.prepare(
            "SELECT commit_seq, timestamp_ms, parent_hash, tx_root, commit_hash, merkle_root, operation_count, delta_payload \
             FROM _lilia_ledger ORDER BY commit_seq ASC;",
        )?;

        while stmt.step()? {
            let seq = stored_version(stmt.column_int64(0));
            let ts = stmt.column_int64(1);
            let parent: [u8; 32] = stmt.column_blob(2).try_into().unwrap_or([0u8; 32]);
            let tx_root: [u8; 32] = stmt.column_blob(3).try_into().unwrap_or([0u8; 32]);
            let commit_hash: [u8; 32] = stmt.column_blob(4).try_into().unwrap_or([0u8; 32]);
            let _merkle_root: [u8; 32] = stmt.column_blob(5).try_into().unwrap_or([0u8; 32]);
            let op_count = stored_count(stmt.column_int64(6));
            let delta_payload = stmt.column_blob(7);

            if seq != expected_seq {
                return Ok(LedgerVerificationReport {
                    is_valid: false,
                    commit_count: seq.saturating_sub(1),
                    leaves_verified: 0,
                    merkle_root: root,
                    error: Some(format!(
                        "DAG sequence broken: expected {expected_seq}, found {seq}"
                    )),
                });
            }

            if parent != expected_parent {
                return Ok(LedgerVerificationReport {
                    is_valid: false,
                    commit_count: seq.saturating_sub(1),
                    leaves_verified: 0,
                    merkle_root: root,
                    error: Some(format!("Parent hash mismatch at commit {seq}")),
                });
            }

            let recomputed = compute_commit_hash(seq, ts, &parent, &tx_root, op_count);
            if recomputed != commit_hash {
                return Ok(LedgerVerificationReport {
                    is_valid: false,
                    commit_count: seq.saturating_sub(1),
                    leaves_verified: 0,
                    merkle_root: root,
                    error: Some(format!("Commit hash mismatch at commit {seq}")),
                });
            }

            let delta_entries: Vec<LedgerDeltaEntry> = match serde_json::from_slice(delta_payload) {
                Ok(entries) => entries,
                Err(e) => {
                    return Ok(LedgerVerificationReport {
                        is_valid: false,
                        commit_count: seq.saturating_sub(1),
                        leaves_verified: 0,
                        merkle_root: root,
                        error: Some(format!("Corrupted delta payload at commit {seq}: {e}")),
                    });
                }
            };
            let leaf_hashes: Vec<[u8; 32]> = delta_entries
                .iter()
                .map(LedgerDeltaEntry::compute_leaf_hash)
                .collect();
            let tx_tree = MerkleTree::build(&leaf_hashes);
            if tx_tree.root() != tx_root {
                return Ok(LedgerVerificationReport {
                    is_valid: false,
                    commit_count: seq.saturating_sub(1),
                    leaves_verified: 0,
                    merkle_root: root,
                    error: Some(format!("Tx root mismatch at commit {seq}")),
                });
            }

            commit_hashes.push(commit_hash);
            expected_parent = commit_hash;
            expected_seq += 1;
        }

        let acc = LedgerAccumulator::from_commits(commit_hashes);
        if commit_count > 0 && acc.root() != root {
            return Ok(LedgerVerificationReport {
                is_valid: false,
                commit_count,
                leaves_verified: 0,
                merkle_root: root,
                error: Some("Accumulator root does not match latest merkle root".into()),
            });
        }

        // 2. Verify live rows
        let leaves_verified = match Self::verify_live_state(conn) {
            Ok(count) => count,
            Err(e) => {
                return Ok(LedgerVerificationReport {
                    is_valid: false,
                    commit_count,
                    leaves_verified: 0,
                    merkle_root: root,
                    error: Some(format!("Live state verification failed: {e}")),
                });
            }
        };

        Ok(LedgerVerificationReport {
            is_valid: true,
            commit_count,
            leaves_verified,
            merkle_root: root,
            error: None,
        })
    }
}

/// Loads the commit chain from `_lilia_ledger`, checking every link when `verify` is set.
fn load_chain(conn: &NativeConnection, verify: bool) -> Result<LedgerInner> {
    let mut commit_hashes = Vec::new();
    let mut last_hash = [0u8; 32];
    let mut last_root = [0u8; 32];
    let mut expected_seq = 1u64;

    let mut stmt = conn.prepare(
        "SELECT commit_seq, timestamp_ms, parent_hash, tx_root, commit_hash, merkle_root, \
         operation_count, delta_payload FROM _lilia_ledger ORDER BY commit_seq ASC;",
    )?;
    while stmt.step()? {
        let row = CommitRow {
            seq: stored_version(stmt.column_int64(0)),
            timestamp_ms: stmt.column_int64(1),
            parent: hash_column(stmt.column_blob(2), "parent_hash")?,
            tx_root: hash_column(stmt.column_blob(3), "tx_root")?,
            commit_hash: hash_column(stmt.column_blob(4), "commit_hash")?,
            operation_count: stmt.column_int64(6),
        };
        if verify {
            row.verify_link(expected_seq, &last_hash)?;
            row.verify_tx_root(stmt.column_blob(7))?;
        }
        expected_seq = row.seq + 1;
        last_hash = row.commit_hash;
        last_root = hash_column(stmt.column_blob(5), "merkle_root")?;
        commit_hashes.push(row.commit_hash);
    }

    let latest_commit_seq = expected_seq - 1;
    let accumulator = LedgerAccumulator::from_commits(commit_hashes);
    if verify && latest_commit_seq > 0 && accumulator.root() != last_root {
        return Err(tamper(format!(
            "Tamper detected in Merkle accumulator: root mismatch (expected {}, found {})",
            hex::encode(accumulator.root()),
            hex::encode(last_root)
        )));
    }
    Ok(LedgerInner {
        latest_commit_seq,
        latest_commit_hash: last_hash,
        latest_merkle_root: if latest_commit_seq == 0 {
            [0u8; 32]
        } else {
            last_root
        },
        accumulator,
    })
}

/// Header of one stored ledger commit.
struct CommitRow {
    seq: u64,
    timestamp_ms: i64,
    parent: [u8; 32],
    tx_root: [u8; 32],
    commit_hash: [u8; 32],
    operation_count: i64,
}

impl CommitRow {
    fn verify_link(&self, expected_seq: u64, expected_parent: &[u8; 32]) -> Result<()> {
        let seq = self.seq;
        if seq != expected_seq {
            return Err(tamper(format!(
                "Tamper detected in Merkle DAG: sequence gap (expected {expected_seq}, found {seq})"
            )));
        }
        if &self.parent != expected_parent {
            return Err(tamper(format!(
                "Tamper detected in Merkle DAG at commit {seq}: broken parent link (expected {}, found {})",
                hex::encode(expected_parent),
                hex::encode(self.parent)
            )));
        }
        let operation_count = u32::try_from(self.operation_count).map_err(|_| {
            tamper(format!(
                "Tamper detected in Merkle DAG at commit {seq}: invalid operation count"
            ))
        })?;
        let recomputed = compute_commit_hash(
            seq,
            self.timestamp_ms,
            &self.parent,
            &self.tx_root,
            operation_count,
        );
        if recomputed != self.commit_hash {
            return Err(tamper(format!(
                "Tamper detected in Merkle DAG at commit {seq}: commit_hash mismatch (expected {}, found {})",
                hex::encode(recomputed),
                hex::encode(self.commit_hash)
            )));
        }
        Ok(())
    }

    fn verify_tx_root(&self, delta_payload: &[u8]) -> Result<()> {
        let seq = self.seq;
        let delta: Vec<LedgerDeltaEntry> = serde_json::from_slice(delta_payload).map_err(|e| {
            tamper(format!(
                "Tamper detected: invalid delta payload at commit {seq}: {e}"
            ))
        })?;
        let leaf_hashes: Vec<[u8; 32]> = delta
            .iter()
            .map(LedgerDeltaEntry::compute_leaf_hash)
            .collect();
        let recomputed = MerkleTree::build(&leaf_hashes).root();
        if recomputed != self.tx_root {
            return Err(tamper(format!(
                "Tamper detected in Merkle DAG at commit {seq}: tx_root mismatch (expected {}, found {})",
                hex::encode(recomputed),
                hex::encode(self.tx_root)
            )));
        }
        Ok(())
    }
}

/// Checks that the leaf recorded for the live row `entry` matches it.
fn verify_live_entry(
    leaf: &mut NativeStatement,
    entry: &LedgerDeltaEntry,
    label: &str,
) -> Result<()> {
    leaf.reset()?;
    leaf.bind_text_static(1, &entry.model)?;
    leaf.bind_text_static(2, &entry.space)?;
    leaf.bind_text_static(3, &entry.id)?;
    leaf.bind_int64(4, stored_int(entry.version))?;
    let target = format!("{}:{}/{}", entry.model, entry.space, entry.id);
    let version = entry.version;
    if !leaf.step()? {
        return Err(tamper(format!(
            "Tamper detected: uncommitted {label} '{target}' (v{version}) found in _lilia_{}",
            entry.model
        )));
    }
    let expected = hash_column(leaf.column_blob(0), "leaf_hash")?;
    let actual = entry.compute_leaf_hash();
    if actual != expected {
        return Err(tamper(format!(
            "Tamper detected: {label} '{target}' (v{version}) was altered! Expected leaf {}, found {}",
            hex::encode(expected),
            hex::encode(actual)
        )));
    }
    Ok(())
}

fn hash_column(blob: &[u8], column: &str) -> Result<[u8; 32]> {
    blob.try_into()
        .map_err(|_| tamper(format!("corrupted {column} blob")))
}

fn tamper(message: String) -> LiliaError {
    LiliaError::new(ErrorCode::TamperDetected, message, false)
}

fn stored_commit_seq(conn: &NativeConnection) -> Result<u64> {
    let mut stmt = conn.prepare("SELECT COALESCE(MAX(commit_seq), 0) FROM _lilia_ledger;")?;
    stmt.step()?;
    Ok(stored_version(stmt.column_int64(0)))
}

/// Versions and sequence numbers are stored as SQLite integers and never negative.
fn stored_version(value: i64) -> u64 {
    u64::try_from(value).unwrap_or_default()
}

fn stored_int(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn stored_index(value: i64) -> usize {
    usize::try_from(value).unwrap_or_default()
}

fn stored_count(value: i64) -> u32 {
    u32::try_from(value).unwrap_or_default()
}

/// TEMP table and triggers created on the writer connection only. A connection opened by any
/// other tool carries no such trigger, so its writes stay unrecorded and the strict verification
/// reports them as tampering.
pub(crate) fn capture_schema() -> String {
    // Statements inside a trigger cannot name a schema: the bare name resolves to the TEMP table.
    // Writing to a `String` cannot fail.
    let mut sql = format!(
        "CREATE TEMP TABLE IF NOT EXISTS {TOUCHED_TABLE}(\
         model TEXT NOT NULL, space TEXT NOT NULL, id NOT NULL, last_version INTEGER NOT NULL, \
         PRIMARY KEY(model, space, id));\n"
    );
    let tables = [
        ("_lilia_json", "json", "space", "id"),
        ("_lilia_kv", "kv", "namespace", "key"),
    ];
    for (table, model, space, id) in tables {
        for (event, rows) in [
            ("INSERT", &["NEW"][..]),
            ("UPDATE", &["OLD", "NEW"][..]),
            ("DELETE", &["OLD"][..]),
        ] {
            let suffix = event[..1].to_ascii_lowercase();
            let _ = write!(
                sql,
                "CREATE TEMP TRIGGER IF NOT EXISTS _lilia_ledger_{model}_{suffix} AFTER {event} ON main.{table} BEGIN"
            );
            for row in rows {
                let _ = write!(
                    sql,
                    " INSERT INTO {TOUCHED_TABLE}(model, space, id, last_version) \
                     SELECT '{model}', {row}.{space}, {row}.{id}, {row}.version \
                     WHERE NOT EXISTS (SELECT 1 FROM {TOUCHED_TABLE} \
                     WHERE model = '{model}' AND space = {row}.{space} AND id = {row}.{id}); \
                     UPDATE {TOUCHED_TABLE} SET last_version = {row}.version \
                     WHERE model = '{model}' AND space = {row}.{space} AND id = {row}.{id};"
                );
            }
            sql.push_str(" END;\n");
        }
    }
    sql
}

/// One `Put` entry per document and entry currently stored, for the genesis commit.
fn live_entries(conn: &NativeConnection) -> Result<Vec<LedgerDeltaEntry>> {
    let mut entries = Vec::new();
    let mut json =
        conn.prepare("SELECT space, id, version, value FROM main._lilia_json ORDER BY space, id;")?;
    while json.step()? {
        entries.push(put_entry(
            "json",
            json.column_text(0),
            json.column_text(1).to_owned(),
            json.column_int64(2),
            json.column_blob(3),
        ));
    }
    let mut kv = conn.prepare(
        "SELECT namespace, key, version, value FROM main._lilia_kv ORDER BY namespace, key;",
    )?;
    while kv.step()? {
        entries.push(put_entry(
            "kv",
            kv.column_text(0),
            hex::encode(kv.column_blob(1)),
            kv.column_int64(2),
            kv.column_blob(3),
        ));
    }
    Ok(entries)
}

fn put_entry(model: &str, space: &str, id: String, version: i64, value: &[u8]) -> LedgerDeltaEntry {
    LedgerDeltaEntry {
        model: model.to_owned(),
        space: space.to_owned(),
        id,
        version: stored_version(version),
        action: LedgerAction::Put,
        payload_hash: *blake3::hash(value).as_bytes(),
    }
}

/// Drains the touched table: the final state of each row, a `Put` of the stored value or a
/// `Delete` one version past the last one removed.
fn touched_entries(conn: &NativeConnection) -> Result<Vec<LedgerDeltaEntry>> {
    let mut touched = Vec::new();
    {
        let mut stmt = conn.prepare(&format!(
            "SELECT model, space, id, last_version FROM temp.{TOUCHED_TABLE} ORDER BY model, space, id;"
        ))?;
        while stmt.step()? {
            let key = if stmt.column_text(0) == "kv" {
                TouchedKey::Kv(stmt.column_blob(2).to_vec())
            } else {
                TouchedKey::Json(stmt.column_text(2).to_owned())
            };
            touched.push(Touched {
                space: stmt.column_text(1).to_owned(),
                key,
                last_version: stored_version(stmt.column_int64(3)),
            });
        }
    }
    if touched.is_empty() {
        return Ok(Vec::new());
    }

    let mut json_row =
        conn.prepare("SELECT version, value FROM main._lilia_json WHERE space = ?1 AND id = ?2;")?;
    let mut kv_row = conn
        .prepare("SELECT version, value FROM main._lilia_kv WHERE namespace = ?1 AND key = ?2;")?;
    let mut entries = Vec::with_capacity(touched.len());
    for row in &touched {
        let (stmt, model, id) = match &row.key {
            TouchedKey::Json(id) => {
                json_row.reset()?;
                json_row.bind_text_static(1, &row.space)?;
                json_row.bind_text_static(2, id)?;
                (&mut json_row, "json", id.clone())
            }
            TouchedKey::Kv(key) => {
                kv_row.reset()?;
                kv_row.bind_text_static(1, &row.space)?;
                kv_row.bind_blob_static(2, key)?;
                (&mut kv_row, "kv", hex::encode(key))
            }
        };
        let entry = if stmt.step()? {
            put_entry(
                model,
                &row.space,
                id,
                stmt.column_int64(0),
                stmt.column_blob(1),
            )
        } else {
            LedgerDeltaEntry {
                model: model.to_owned(),
                space: row.space.clone(),
                id,
                version: row.last_version + 1,
                action: LedgerAction::Delete,
                payload_hash: [0u8; 32],
            }
        };
        entries.push(entry);
    }
    conn.exec(&format!("DELETE FROM temp.{TOUCHED_TABLE};"))?;
    Ok(entries)
}

/// Ledger entries for the operations whose rows the capture triggers do not see: graph, full-text,
/// vector and CRDT writes. JSON and KV operations are recorded from their rows, and memory-tier
/// operations never reach the database file.
pub(crate) fn operation_entries(
    operations: &[BatchOperation],
    results: &[MutationResult],
) -> Vec<LedgerDeltaEntry> {
    use lilia_core::CollectionKind;

    operations
        .iter()
        .zip(results)
        .filter(|(op, res)| {
            res.tier != StorageTier::Memory
                && !matches!(
                    crate::engine::op_collection(op).0,
                    CollectionKind::Json | CollectionKind::Kv
                )
        })
        .map(|(op, res)| match op {
            BatchOperation::GraphSetNode {
                graph,
                id,
                metadata,
                ..
            } => LedgerDeltaEntry {
                model: "graph_node".into(),
                space: graph.clone(),
                id: id.clone(),
                version: res.version.unwrap_or(1),
                action: LedgerAction::Put,
                payload_hash: *blake3::hash(&serde_json::to_vec(metadata).unwrap_or_default())
                    .as_bytes(),
            },
            BatchOperation::GraphDeleteNode { graph, id } => LedgerDeltaEntry {
                model: "graph_node".into(),
                space: graph.clone(),
                id: id.clone(),
                version: 0,
                action: LedgerAction::Delete,
                payload_hash: [0u8; 32],
            },
            _ => {
                let (kind, name) = crate::engine::op_collection(op);
                LedgerDeltaEntry {
                    model: format!("{kind:?}").to_lowercase(),
                    space: name.to_owned(),
                    id: format!("op_{}", res.version.unwrap_or(0)),
                    version: res.version.unwrap_or(1),
                    action: if res.deleted {
                        LedgerAction::Delete
                    } else {
                        LedgerAction::Put
                    },
                    payload_hash: [0u8; 32],
                }
            }
        })
        .collect()
}
