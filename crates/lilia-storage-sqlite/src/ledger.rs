//! Cryptographic Merkle Ledger Manager for `lilia-storage-sqlite`.
//!
//! Enforces atomic cryptographic commit chaining via BLAKE3, maintains the Merkle DAG
//! in `_lilia_ledger` and `_lilia_ledger_leaves`, generates mathematical inclusion proofs,
//! and performs tamper detection on database startup.

use std::sync::RwLock;

use lilia_core::ledger::{
    compute_commit_hash, InclusionProof, LedgerAccumulator, LedgerAction, LedgerCommit,
    LedgerDeltaEntry, LedgerVerificationReport, LedgerVerifyMode, MerkleTree,
};
use lilia_core::{BatchOperation, ErrorCode, LiliaError, MutationResult, Result};
use lilia_sqlite_native::{NativeConnection, NativeTransaction};

#[derive(Debug)]
pub(crate) struct LedgerManager {
    enabled: bool,
    #[allow(dead_code)]
    verify_mode: LedgerVerifyMode,
    inner: RwLock<LedgerInner>,
}

#[derive(Debug, Default)]
struct LedgerInner {
    latest_commit_seq: u64,
    latest_commit_hash: [u8; 32],
    latest_merkle_root: [u8; 32],
    accumulator: LedgerAccumulator,
}

impl LedgerManager {
    /// Opens the ledger manager and verifies the database integrity according to `verify_mode`.
    pub(crate) fn open(
        conn: &NativeConnection,
        enabled: bool,
        verify_mode: LedgerVerifyMode,
    ) -> Result<Self> {
        if !enabled {
            return Ok(Self {
                enabled: false,
                verify_mode: LedgerVerifyMode::None,
                inner: RwLock::new(LedgerInner::default()),
            });
        }

        let mut commit_hashes = Vec::new();
        let mut last_seq = 0u64;
        let mut last_hash = [0u8; 32];
        let mut last_root = [0u8; 32];

        // 1. Load commit DAG
        if let Ok(mut stmt) = conn.prepare(
            "SELECT commit_seq, timestamp_ms, parent_hash, tx_root, commit_hash, merkle_root, \
             operation_count, delta_payload FROM _lilia_ledger ORDER BY commit_seq ASC;",
        ) {
            let mut expected_seq = 1u64;
            let mut expected_parent = [0u8; 32];

            while stmt.step()? {
                let seq = stmt.column_int64(0) as u64;
                let ts = stmt.column_int64(1);
                let parent: [u8; 32] = stmt.column_blob(2).try_into().map_err(|_| {
                    LiliaError::new(ErrorCode::TamperDetected, "corrupted parent_hash blob", false)
                })?;
                let tx_root: [u8; 32] = stmt.column_blob(3).try_into().map_err(|_| {
                    LiliaError::new(ErrorCode::TamperDetected, "corrupted tx_root blob", false)
                })?;
                let commit_hash: [u8; 32] = stmt.column_blob(4).try_into().map_err(|_| {
                    LiliaError::new(ErrorCode::TamperDetected, "corrupted commit_hash blob", false)
                })?;
                let merkle_root: [u8; 32] = stmt.column_blob(5).try_into().map_err(|_| {
                    LiliaError::new(ErrorCode::TamperDetected, "corrupted merkle_root blob", false)
                })?;
                let op_count = stmt.column_int64(6) as u32;
                let delta_payload = stmt.column_blob(7);

                // Verify DAG sequence and parent link
                if verify_mode != LedgerVerifyMode::None {
                    if seq != expected_seq {
                        return Err(LiliaError::new(
                            ErrorCode::TamperDetected,
                            format!(
                                "Tamper detected in Merkle DAG: sequence gap (expected {expected_seq}, found {seq})"
                            ),
                            false,
                        ));
                    }
                    if parent != expected_parent {
                        return Err(LiliaError::new(
                            ErrorCode::TamperDetected,
                            format!(
                                "Tamper detected in Merkle DAG at commit {seq}: broken parent link (expected {}, found {})",
                                hex::encode(expected_parent),
                                hex::encode(parent)
                            ),
                            false,
                        ));
                    }

                    // Recompute commit hash
                    let recomputed_commit_hash =
                        compute_commit_hash(seq, ts, &parent, &tx_root, op_count);
                    if recomputed_commit_hash != commit_hash {
                        return Err(LiliaError::new(
                            ErrorCode::TamperDetected,
                            format!(
                                "Tamper detected in Merkle DAG at commit {seq}: commit_hash mismatch (expected {}, found {})",
                                hex::encode(recomputed_commit_hash),
                                hex::encode(commit_hash)
                            ),
                            false,
                        ));
                    }

                    // Reconstruct TxMerkleTree from delta payload
                    let delta_entries: Vec<LedgerDeltaEntry> = serde_json::from_slice(delta_payload)
                        .map_err(|e| {
                            LiliaError::new(
                                ErrorCode::TamperDetected,
                                format!("Tamper detected: invalid delta payload at commit {seq}: {e}"),
                                false,
                            )
                        })?;
                    let leaf_hashes: Vec<[u8; 32]> = delta_entries
                        .iter()
                        .map(LedgerDeltaEntry::compute_leaf_hash)
                        .collect();
                    let tx_tree = MerkleTree::build(&leaf_hashes);
                    if tx_tree.root() != tx_root {
                        return Err(LiliaError::new(
                            ErrorCode::TamperDetected,
                            format!(
                                "Tamper detected in Merkle DAG at commit {seq}: tx_root mismatch (expected {}, found {})",
                                hex::encode(tx_tree.root()),
                                hex::encode(tx_root)
                            ),
                            false,
                        ));
                    }
                }

                expected_seq = seq + 1;
                expected_parent = commit_hash;
                last_seq = seq;
                last_hash = commit_hash;
                last_root = merkle_root;
                commit_hashes.push(commit_hash);
            }
        }

        let accumulator = LedgerAccumulator::from_commits(commit_hashes);
        if verify_mode != LedgerVerifyMode::None && last_seq > 0 {
            if accumulator.root() != last_root {
                return Err(LiliaError::new(
                    ErrorCode::TamperDetected,
                    format!(
                        "Tamper detected in Merkle accumulator: root mismatch (expected {}, found {})",
                        hex::encode(accumulator.root()),
                        hex::encode(last_root)
                    ),
                    false,
                ));
            }
        }

        let mgr = Self {
            enabled,
            verify_mode,
            inner: RwLock::new(LedgerInner {
                latest_commit_seq: last_seq,
                latest_commit_hash: last_hash,
                latest_merkle_root: if last_seq == 0 { [0u8; 32] } else { last_root },
                accumulator,
            }),
        };

        // 2. Strict live state verification if requested
        if verify_mode == LedgerVerifyMode::Strict && last_seq > 0 {
            mgr.verify_live_state(conn)?;
        }

        Ok(mgr)
    }

    /// Verifies that all live rows in `_lilia_json` and `_lilia_kv` match their cryptographic commitments.
    pub(crate) fn verify_live_state(&self, conn: &NativeConnection) -> Result<u64> {
        let mut count = 0u64;

        // Check _lilia_json
        let mut stmt = conn.prepare("SELECT space, id, version, value FROM _lilia_json;")?;
        while stmt.step()? {
            let space = stmt.column_text(0);
            let id = stmt.column_text(1);
            let version = stmt.column_int64(2) as u64;
            let value_blob = stmt.column_blob(3);

            let actual_payload_hash = *blake3::hash(value_blob).as_bytes();

            // Find leaf in _lilia_ledger_leaves
            let mut leaf_stmt = conn.prepare(
                "SELECT leaf_hash FROM _lilia_ledger_leaves WHERE model = 'json' AND space = ?1 AND id = ?2 AND version = ?3;",
            )?;
            leaf_stmt.bind_text_static(1, space)?;
            leaf_stmt.bind_text_static(2, id)?;
            leaf_stmt.bind_int64(3, version as i64)?;

            if leaf_stmt.step()? {
                let expected_leaf_hash: [u8; 32] = leaf_stmt.column_blob(0).try_into().map_err(|_| {
                    LiliaError::new(ErrorCode::TamperDetected, "corrupted leaf_hash blob", false)
                })?;

                let entry = LedgerDeltaEntry {
                    model: "json".into(),
                    space: space.into(),
                    id: id.into(),
                    version,
                    action: LedgerAction::Put,
                    payload_hash: actual_payload_hash,
                };
                let actual_leaf_hash = entry.compute_leaf_hash();

                if actual_leaf_hash != expected_leaf_hash {
                    return Err(LiliaError::new(
                        ErrorCode::TamperDetected,
                        format!(
                            "Tamper detected: document 'json:{space}/{id}' (v{version}) payload was altered! Expected leaf {}, found {}",
                            hex::encode(expected_leaf_hash),
                            hex::encode(actual_leaf_hash)
                        ),
                        false,
                    ));
                }
            } else {
                return Err(LiliaError::new(
                    ErrorCode::TamperDetected,
                    format!("Tamper detected: uncommitted document 'json:{space}/{id}' (v{version}) found in _lilia_json"),
                    false,
                ));
            }
            count += 1;
        }

        // Check _lilia_kv
        let mut stmt_kv = conn.prepare("SELECT namespace, key, value, version FROM _lilia_kv;")?;
        while stmt_kv.step()? {
            let namespace = stmt_kv.column_text(0);
            let key = stmt_kv.column_blob(1);
            let value = stmt_kv.column_blob(2);
            let version = stmt_kv.column_int64(3) as u64;

            let key_str = hex::encode(key);
            let actual_payload_hash = *blake3::hash(value).as_bytes();

            let mut leaf_stmt = conn.prepare(
                "SELECT leaf_hash FROM _lilia_ledger_leaves WHERE model = 'kv' AND space = ?1 AND id = ?2 AND version = ?3;",
            )?;
            leaf_stmt.bind_text_static(1, namespace)?;
            leaf_stmt.bind_text_static(2, &key_str)?;
            leaf_stmt.bind_int64(3, version as i64)?;

            if leaf_stmt.step()? {
                let expected_leaf_hash: [u8; 32] = leaf_stmt.column_blob(0).try_into().map_err(|_| {
                    LiliaError::new(ErrorCode::TamperDetected, "corrupted leaf_hash blob", false)
                })?;

                let entry = LedgerDeltaEntry {
                    model: "kv".into(),
                    space: namespace.into(),
                    id: key_str.clone(),
                    version,
                    action: LedgerAction::Put,
                    payload_hash: actual_payload_hash,
                };
                let actual_leaf_hash = entry.compute_leaf_hash();

                if actual_leaf_hash != expected_leaf_hash {
                    return Err(LiliaError::new(
                        ErrorCode::TamperDetected,
                        format!(
                            "Tamper detected: KV entry 'kv:{namespace}/{key_str}' (v{version}) was altered! Expected leaf {}, found {}",
                            hex::encode(expected_leaf_hash),
                            hex::encode(actual_leaf_hash)
                        ),
                        false,
                    ));
                }
            } else {
                return Err(LiliaError::new(
                    ErrorCode::TamperDetected,
                    format!("Tamper detected: uncommitted KV entry 'kv:{namespace}/{key_str}' (v{version}) found in _lilia_kv"),
                    false,
                ));
            }
            count += 1;
        }

        Ok(count)
    }

    /// Records an atomic commit delta in `_lilia_ledger` and `_lilia_ledger_leaves`.
    pub(crate) fn record_commit(
        &self,
        tx: &mut NativeTransaction<'_>,
        operations: &[BatchOperation],
        results: &[MutationResult],
        now: i64,
    ) -> Result<[u8; 32]> {
        if !self.enabled || operations.is_empty() {
            return Ok(self.root());
        }

        // Build delta entries
        let mut delta_entries = Vec::with_capacity(operations.len());
        for (op, res) in operations.iter().zip(results) {
            match op {
                BatchOperation::JsonPut { space, id, .. }
                | BatchOperation::JsonPutRaw { space, id, .. }
                | BatchOperation::JsonPutHlc { space, id, .. } => {
                    let version = res.version.unwrap_or(1);
                    // Fetch stored value blob directly from _lilia_json
                    let mut stmt = tx.prepare(
                        "SELECT value FROM _lilia_json WHERE space = ?1 AND id = ?2;",
                    )?;
                    stmt.bind_text_static(1, space)?;
                    stmt.bind_text_static(2, id)?;
                    let payload_hash = if stmt.step()? {
                        *blake3::hash(stmt.column_blob(0)).as_bytes()
                    } else {
                        [0u8; 32]
                    };
                    delta_entries.push(LedgerDeltaEntry {
                        model: "json".into(),
                        space: space.clone(),
                        id: id.clone(),
                        version,
                        action: LedgerAction::Put,
                        payload_hash,
                    });
                }
                BatchOperation::JsonDelete { space, id, .. } => {
                    let version = res.version.unwrap_or(0);
                    delta_entries.push(LedgerDeltaEntry {
                        model: "json".into(),
                        space: space.clone(),
                        id: id.clone(),
                        version,
                        action: LedgerAction::Delete,
                        payload_hash: [0u8; 32],
                    });
                }
                BatchOperation::KvSet {
                    namespace,
                    key,
                    value,
                    ..
                } => {
                    let version = res.version.unwrap_or(1);
                    let mut stmt = tx.prepare(
                        "SELECT value FROM _lilia_kv WHERE namespace = ?1 AND key = ?2;",
                    )?;
                    stmt.bind_text_static(1, namespace)?;
                    stmt.bind_blob_static(2, key)?;
                    let payload_hash = if stmt.step()? {
                        *blake3::hash(stmt.column_blob(0)).as_bytes()
                    } else {
                        *blake3::hash(value).as_bytes()
                    };
                    delta_entries.push(LedgerDeltaEntry {
                        model: "kv".into(),
                        space: namespace.clone(),
                        id: hex::encode(key),
                        version,
                        action: LedgerAction::Put,
                        payload_hash,
                    });
                }
                BatchOperation::KvDelete { namespace, key, .. } => {
                    let version = res.version.unwrap_or(0);
                    delta_entries.push(LedgerDeltaEntry {
                        model: "kv".into(),
                        space: namespace.clone(),
                        id: hex::encode(key),
                        version,
                        action: LedgerAction::Delete,
                        payload_hash: [0u8; 32],
                    });
                }
                BatchOperation::GraphSetNode {
                    graph,
                    id,
                    metadata,
                    ..
                } => {
                    let meta_bytes = serde_json::to_vec(metadata).unwrap_or_default();
                    delta_entries.push(LedgerDeltaEntry {
                        model: "graph_node".into(),
                        space: graph.clone(),
                        id: id.clone(),
                        version: res.version.unwrap_or(1),
                        action: LedgerAction::Put,
                        payload_hash: *blake3::hash(&meta_bytes).as_bytes(),
                    });
                }
                BatchOperation::GraphDeleteNode { graph, id } => {
                    delta_entries.push(LedgerDeltaEntry {
                        model: "graph_node".into(),
                        space: graph.clone(),
                        id: id.clone(),
                        version: 0,
                        action: LedgerAction::Delete,
                        payload_hash: [0u8; 32],
                    });
                }
                _ => {
                    // Other operation models
                    let (kind_str, name_str) = crate::engine::op_collection(op);
                    delta_entries.push(LedgerDeltaEntry {
                        model: format!("{kind_str:?}").to_lowercase(),
                        space: name_str.to_string(),
                        id: format!("op_{}", res.version.unwrap_or(0)),
                        version: res.version.unwrap_or(1),
                        action: if res.deleted {
                            LedgerAction::Delete
                        } else {
                            LedgerAction::Put
                        },
                        payload_hash: [0u8; 32],
                    });
                }
            }
        }

        if delta_entries.is_empty() {
            return Ok(self.root());
        }

        let leaf_hashes: Vec<[u8; 32]> = delta_entries
            .iter()
            .map(LedgerDeltaEntry::compute_leaf_hash)
            .collect();
        let tx_tree = MerkleTree::build(&leaf_hashes);
        let tx_root = tx_tree.root();

        let mut inner = self.inner.write().map_err(|_| {
            LiliaError::new(ErrorCode::Storage, "ledger lock poisoned", false)
        })?;

        let commit_seq = inner.latest_commit_seq + 1;
        let parent_hash = inner.latest_commit_hash;
        let op_count = delta_entries.len() as u32;

        let commit_hash = compute_commit_hash(commit_seq, now, &parent_hash, &tx_root, op_count);
        let merkle_root = inner.accumulator.append(commit_hash);

        let delta_payload = serde_json::to_vec(&delta_entries).map_err(|e| {
            LiliaError::new(ErrorCode::Storage, format!("failed to serialize delta: {e}"), false)
        })?;

        // 1. Insert into _lilia_ledger
        let mut stmt_commit = tx.prepare(
            "INSERT INTO _lilia_ledger(commit_seq, timestamp_ms, parent_hash, tx_root, commit_hash, merkle_root, operation_count, delta_payload, metadata) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL);",
        )?;
        stmt_commit.bind_int64(1, commit_seq as i64)?;
        stmt_commit.bind_int64(2, now)?;
        stmt_commit.bind_blob_static(3, &parent_hash)?;
        stmt_commit.bind_blob_static(4, &tx_root)?;
        stmt_commit.bind_blob_static(5, &commit_hash)?;
        stmt_commit.bind_blob_static(6, &merkle_root)?;
        stmt_commit.bind_int64(7, i64::from(op_count))?;
        stmt_commit.bind_blob_static(8, &delta_payload)?;
        stmt_commit.step()?;

        // 2. Insert leaves into _lilia_ledger_leaves
        let mut stmt_leaf = tx.prepare(
            "INSERT OR REPLACE INTO _lilia_ledger_leaves(model, space, id, version, commit_seq, leaf_index, leaf_hash) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7);",
        )?;
        for (idx, (entry, leaf_hash)) in delta_entries.iter().zip(&leaf_hashes).enumerate() {
            stmt_leaf.reset()?;
            stmt_leaf.bind_text_static(1, &entry.model)?;
            stmt_leaf.bind_text_static(2, &entry.space)?;
            stmt_leaf.bind_text_static(3, &entry.id)?;
            stmt_leaf.bind_int64(4, entry.version as i64)?;
            stmt_leaf.bind_int64(5, commit_seq as i64)?;
            stmt_leaf.bind_int64(6, idx as i64)?;
            stmt_leaf.bind_blob_static(7, leaf_hash)?;
            stmt_leaf.step()?;
        }

        // Update in-memory state
        inner.latest_commit_seq = commit_seq;
        inner.latest_commit_hash = commit_hash;
        inner.latest_merkle_root = merkle_root;

        Ok(merkle_root)
    }

    /// Returns the current Merkle root of the database.
    pub(crate) fn root(&self) -> [u8; 32] {
        self.inner
            .read()
            .map(|i| i.latest_merkle_root)
            .unwrap_or([0u8; 32])
    }

    /// Returns the latest commit header.
    pub(crate) fn latest_commit(&self, conn: &NativeConnection) -> Result<Option<LedgerCommit>> {
        let seq = self.inner.read().map(|i| i.latest_commit_seq).unwrap_or(0);
        if seq == 0 {
            return Ok(None);
        }
        self.commit_by_seq(conn, seq)
    }

    /// Returns a commit header by its 1-based sequence number.
    pub(crate) fn commit_by_seq(
        &self,
        conn: &NativeConnection,
        seq: u64,
    ) -> Result<Option<LedgerCommit>> {
        let mut stmt = conn.prepare(
            "SELECT commit_seq, timestamp_ms, parent_hash, tx_root, commit_hash, merkle_root, operation_count, metadata \
             FROM _lilia_ledger WHERE commit_seq = ?1;",
        )?;
        stmt.bind_int64(1, seq as i64)?;
        if !stmt.step()? {
            return Ok(None);
        }

        let commit_seq = stmt.column_int64(0) as u64;
        let timestamp_ms = stmt.column_int64(1);
        let parent_hash: [u8; 32] = stmt.column_blob(2).try_into().map_err(|_| {
            LiliaError::new(ErrorCode::Storage, "invalid parent_hash blob", false)
        })?;
        let tx_root: [u8; 32] = stmt.column_blob(3).try_into().map_err(|_| {
            LiliaError::new(ErrorCode::Storage, "invalid tx_root blob", false)
        })?;
        let commit_hash: [u8; 32] = stmt.column_blob(4).try_into().map_err(|_| {
            LiliaError::new(ErrorCode::Storage, "invalid commit_hash blob", false)
        })?;
        let merkle_root: [u8; 32] = stmt.column_blob(5).try_into().map_err(|_| {
            LiliaError::new(ErrorCode::Storage, "invalid merkle_root blob", false)
        })?;
        let operation_count = stmt.column_int64(6) as u32;
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
            stmt.bind_int64(3, v as i64)?;
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
            (v, stmt.column_int64(1) as u64, stmt.column_int64(2) as usize, h)
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
            let v = stmt.column_int64(0) as u64;
            let h: [u8; 32] = stmt.column_blob(3).try_into().map_err(|_| {
                LiliaError::new(ErrorCode::Storage, "invalid leaf_hash blob", false)
            })?;
            (v, stmt.column_int64(1) as u64, stmt.column_int64(2) as usize, h)
        };

        self.build_proof(conn, format!("json:{space}/{id}:v{ver}"), commit_seq, leaf_idx, leaf_hash)
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
            stmt.bind_int64(3, v as i64)?;
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
            (v, stmt.column_int64(1) as u64, stmt.column_int64(2) as usize, h)
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
            let v = stmt.column_int64(0) as u64;
            let h: [u8; 32] = stmt.column_blob(3).try_into().map_err(|_| {
                LiliaError::new(ErrorCode::Storage, "invalid leaf_hash blob", false)
            })?;
            (v, stmt.column_int64(1) as u64, stmt.column_int64(2) as usize, h)
        };

        self.build_proof(conn, format!("kv:{namespace}/{key_str}:v{ver}"), commit_seq, leaf_idx, leaf_hash)
    }

    /// Exports an inclusion proof for an entire commit.
    pub(crate) fn prove_commit(
        &self,
        conn: &NativeConnection,
        commit_seq: u64,
    ) -> Result<InclusionProof> {
        let commit = self.commit_by_seq(conn, commit_seq)?.ok_or_else(|| {
            LiliaError::new(
                ErrorCode::NotFound,
                format!("commit sequence {commit_seq} not found in ledger"),
                false,
            )
        })?;

        let inner = self.inner.read().map_err(|_| {
            LiliaError::new(ErrorCode::Storage, "ledger lock poisoned", false)
        })?;

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
        stmt.bind_int64(1, commit_seq as i64)?;
        if !stmt.step()? {
            return Err(LiliaError::new(
                ErrorCode::NotFound,
                format!("commit {commit_seq} not found for proof"),
                false,
            ));
        }

        let timestamp_ms = stmt.column_int64(0);
        let tx_root: [u8; 32] = stmt.column_blob(1).try_into().map_err(|_| {
            LiliaError::new(ErrorCode::Storage, "invalid tx_root blob", false)
        })?;
        let commit_hash: [u8; 32] = stmt.column_blob(2).try_into().map_err(|_| {
            LiliaError::new(ErrorCode::Storage, "invalid commit_hash blob", false)
        })?;
        let delta_payload = stmt.column_blob(3);

        let delta_entries: Vec<LedgerDeltaEntry> = serde_json::from_slice(delta_payload).map_err(|e| {
            LiliaError::new(ErrorCode::Storage, format!("invalid delta payload: {e}"), false)
        })?;
        let leaf_hashes: Vec<[u8; 32]> = delta_entries
            .iter()
            .map(LedgerDeltaEntry::compute_leaf_hash)
            .collect();
        let tx_tree = MerkleTree::build(&leaf_hashes);
        let tx_audit_path = tx_tree.audit_path(leaf_idx);

        let inner = self.inner.read().map_err(|_| {
            LiliaError::new(ErrorCode::Storage, "ledger lock poisoned", false)
        })?;

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
    pub(crate) fn verify_ledger(&self, conn: &NativeConnection) -> Result<LedgerVerificationReport> {
        let commit_count = self.inner.read().map(|i| i.latest_commit_seq).unwrap_or(0);
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
            let seq = stmt.column_int64(0) as u64;
            let ts = stmt.column_int64(1);
            let parent: [u8; 32] = stmt.column_blob(2).try_into().unwrap_or([0u8; 32]);
            let tx_root: [u8; 32] = stmt.column_blob(3).try_into().unwrap_or([0u8; 32]);
            let commit_hash: [u8; 32] = stmt.column_blob(4).try_into().unwrap_or([0u8; 32]);
            let _merkle_root: [u8; 32] = stmt.column_blob(5).try_into().unwrap_or([0u8; 32]);
            let op_count = stmt.column_int64(6) as u32;
            let delta_payload = stmt.column_blob(7);

            if seq != expected_seq {
                return Ok(LedgerVerificationReport {
                    is_valid: false,
                    commit_count: seq.saturating_sub(1),
                    leaves_verified: 0,
                    merkle_root: root,
                    error: Some(format!("DAG sequence broken: expected {expected_seq}, found {seq}")),
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
        let leaves_verified = match self.verify_live_state(conn) {
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
