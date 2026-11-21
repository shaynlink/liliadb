//! The ledger: a tamper-evident history of every record write, as a Merkle Mountain Range of
//! commits stored next to the data.
//!
//! Each committed batch (or transaction) that writes records becomes one ledger commit: the
//! deltas it applied (model, collection, id, version, action, payload hash), their Merkle root,
//! and a hash chained to the previous commit. The commit hash is appended to the range, whose
//! root attests the whole history; proofs of a commit or of a live record take `O(log n)` reads.
//!
//! The ledger commit is written in the same engine batch as the data, so neither exists without
//! the other. Versions are engine sequence numbers, known only at commit; ledger writers therefore
//! commit through one leader, which merges the pending batches into one engine batch pinned to
//! the next sequence number ([`lilia_engine::WriteBatch::at_sequence`]), so it knows every
//! version it records. Writers keep sharing one journal sync per group.

pub mod mmr;
mod record;
mod verify;

use std::collections::HashMap;
use std::ops::Bound;
use std::sync::Arc;

use bytes::Bytes;
use lilia_core::ledger::{hex_bytes, AuditHop};
use lilia_core::{
    compute_commit_hash, verify_audit_path, LedgerAction, LedgerCommit, LedgerDeltaEntry,
};
use lilia_engine::{Db, GroupCommit, Member, Snapshot, WriteBatch};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use self::mmr::{proof_positions, Hash, MmrProof, Peaks};
use self::record::{tx_tree, CommitRecord, Head};
use crate::batch::Outcome;
use crate::error::{corruption, invalid, Error, Result};
use crate::keys;
use crate::record::now_ms;
use crate::write::{Model, Plan};

/// Result of one commit attempt of a plan.
#[derive(Debug, Clone)]
pub enum Attempt {
    /// Committed; its writes start at sequence `first`.
    Committed { first: u64, outcomes: Vec<Outcome> },
    /// An internal expectation failed: plan the batch again.
    Retry,
}

/// A plan waiting for its ledger group.
type Pending = Member<Plan, Result<Attempt>>;

/// The ledger of a database.
#[derive(Debug)]
pub struct Ledger {
    /// The frontier; only the group leader locks it.
    head: Mutex<Head>,
    queue: GroupCommit<Plan, Result<Attempt>>,
    /// Bytes of plans one group may take.
    max_group_bytes: usize,
}

/// Proof that a commit is in the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitProof {
    pub commit: LedgerCommit,
    pub proof: MmrProof,
    /// Root of the ledger the proof was made against.
    #[serde(with = "hex_bytes")]
    pub root: Hash,
}

/// Proof that the current version of a record was written by a commit of the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordProof {
    pub delta: LedgerDeltaEntry,
    /// Path from the delta's leaf hash to the commit's transaction root.
    pub tx_path: Vec<AuditHop>,
    pub commit: LedgerCommit,
    pub proof: MmrProof,
    #[serde(with = "hex_bytes")]
    pub root: Hash,
}

fn commit_is_sound(commit: &LedgerCommit, proof: &MmrProof) -> bool {
    commit.commit_seq == proof.leaf_index + 1
        && compute_commit_hash(
            commit.commit_seq,
            commit.timestamp_ms,
            &commit.parent_hash,
            &commit.tx_root,
            commit.operation_count,
        ) == commit.commit_hash
}

impl CommitProof {
    /// Whether the commit hashes to its stated hash and that hash is in the ledger of `root`.
    #[must_use]
    pub fn verify(&self, root: &Hash) -> bool {
        commit_is_sound(&self.commit, &self.proof)
            && self.proof.verify(&self.commit.commit_hash, root)
    }
}

impl RecordProof {
    /// Whether the delta is in its commit and the commit is in the ledger of `root`.
    #[must_use]
    pub fn verify(&self, root: &Hash) -> bool {
        verify_audit_path(&self.delta.compute_leaf_hash(), &self.tx_path) == self.commit.tx_root
            && commit_is_sound(&self.commit, &self.proof)
            && self.proof.verify(&self.commit.commit_hash, root)
    }
}

fn read_head(engine: &Db, snapshot: Option<&Snapshot>) -> Result<Head> {
    let raw = match snapshot {
        Some(snapshot) => engine.get_at(snapshot, keys::LEDGER_HEAD)?.map(|v| v.value),
        None => engine.get(keys::LEDGER_HEAD)?,
    };
    Head::decode(&raw.ok_or_else(|| corruption("the ledger head is missing"))?)
}

fn read_node(engine: &Db, position: u64) -> Result<Hash> {
    let raw = engine
        .get(&keys::ledger_node(position))?
        .ok_or_else(|| corruption(format!("ledger node {position} is missing")))?;
    <[u8; 32]>::try_from(raw.as_ref())
        .map_err(|_| corruption(format!("ledger node {position} is not a hash")))
}

fn read_commit(engine: &Db, index: u64) -> Result<CommitRecord> {
    let raw = engine
        .get(&keys::ledger_commit(index))?
        .ok_or_else(|| corruption(format!("ledger commit {} is missing", index + 1)))?;
    CommitRecord::decode(&raw)
}

/// Model, collection and id of the record stored at `key`, as ledger deltas name them.
fn record_identity(key: &[u8]) -> Option<(Model, String, String)> {
    let (&tag, rest) = key.split_first()?;
    let split = rest.iter().position(|&b| b == 0)?;
    let space = String::from_utf8(rest[..split].to_vec()).ok()?;
    let record = &rest[split + 1..];
    match tag {
        keys::KV => Some((Model::Kv, space, hex::encode(record))),
        keys::DOC => Some((Model::Json, space, String::from_utf8(record.to_vec()).ok()?)),
        keys::GRAPH_NODE => Some((
            Model::GraphNode,
            space,
            String::from_utf8(record.to_vec()).ok()?,
        )),
        keys::EDGE_OUT => Some((
            Model::GraphEdge,
            space,
            crate::graph::edge_identity(record)?,
        )),
        keys::VECTOR => Some((
            Model::Vector,
            space,
            String::from_utf8(record.to_vec()).ok()?,
        )),
        keys::TS_CHUNK => Some((
            Model::TimeSeries,
            space,
            crate::timeseries::identity(record)?,
        )),
        keys::GEO_FENCE | keys::GEO_POINT => {
            let (model, id) = crate::geo::identity(tag, record)?;
            Some((model, space, id))
        }
        keys::CRDT_COUNTER | keys::CRDT_SET | keys::CRDT_FIELD => {
            let (model, id) = crate::crdt::identity(tag, record)?;
            Some((model, space, id))
        }
        keys::FTS_DOC | keys::FTS_DICTIONARY => {
            let (model, id) = crate::fts::identity(tag, record)?;
            Some((model, space, id))
        }
        _ => None,
    }
}

impl Ledger {
    /// Opens the ledger stored in `engine`, starting one if `enable` and there is none. A
    /// ledger group takes plans of at most `max_group_bytes` in total.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidArgument`] when the database keeps a ledger and `enable` is false: a
    /// ledger cannot be turned off. Engine errors.
    pub fn open(engine: &Db, enable: bool, max_group_bytes: usize) -> Result<Option<Self>> {
        if engine.get(keys::LEDGER_HEAD)?.is_some() {
            if !enable {
                return Err(invalid("this database keeps a ledger, which cannot be turned off: open it with the ledger enabled"));
            }
            let head = read_head(engine, None)?;
            return Ok(Some(Self {
                head: Mutex::new(head),
                queue: GroupCommit::default(),
                max_group_bytes,
            }));
        }
        if !enable {
            return Ok(None);
        }
        loop {
            let at = engine.last_sequence() + 1;
            let head = Head {
                start_sequence: at + 1,
                peaks: Peaks::default(),
                last_commit: [0; 32],
            };
            let mut batch = WriteBatch::new();
            batch
                .at_sequence(at)
                .put(Bytes::from_static(keys::LEDGER_HEAD), head.encode());
            match engine.write(batch) {
                Ok(_) => {
                    return Ok(Some(Self {
                        head: Mutex::new(head),
                        queue: GroupCommit::default(),
                        max_group_bytes,
                    }))
                }
                Err(lilia_engine::Error::SequenceMoved { .. }) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }

    /// Commits `plan` with its ledger commit, in a group with the plans of concurrent writers.
    ///
    /// # Errors
    ///
    /// The plan's strict conflicts, engine errors.
    pub fn commit(&self, engine: &Db, plan: Plan) -> Result<Attempt> {
        let weight = plan.approximate_bytes();
        self.queue
            .commit(plan, weight, self.max_group_bytes, |group| {
                Self::commit_group(engine, &mut self.head.lock(), group);
            })
            .unwrap_or_else(|| Err(corruption("a ledger commit lost its result")))
    }

    /// Checks a plan's expectations against the engine and the writes of earlier plans of the
    /// group; `Some` is the plan's result when one fails.
    fn check(
        engine: &Db,
        plan: &Plan,
        written: &HashMap<Bytes, Option<u64>>,
    ) -> Result<Option<Result<Attempt>>> {
        for (key, expected) in &plan.expected {
            let actual = match written.get(key) {
                Some(&version) => version,
                None => engine.get_versioned(key)?.map(|v| v.version),
            };
            if actual != expected.version {
                return Ok(Some(
                    plan.conflict(key, expected.version, actual)
                        .map_or(Ok(Attempt::Retry), Err),
                ));
            }
        }
        Ok(None)
    }

    /// Appends the ledger commit of `plan`, whose writes start at sequence `base`, to `frontier`
    /// and `batch`.
    fn append(
        frontier: &mut Head,
        batch: &mut WriteBatch,
        plan: &Plan,
        base: u64,
        timestamp_ms: i64,
    ) {
        let deltas = plan
            .deltas
            .iter()
            .map(|d| LedgerDeltaEntry {
                model: d.model.name().to_owned(),
                space: d.space.clone(),
                id: d.id.clone(),
                version: base + d.op as u64,
                action: if d.payload.is_some() {
                    LedgerAction::Put
                } else {
                    LedgerAction::Delete
                },
                payload_hash: d.payload.unwrap_or([0; 32]),
            })
            .collect();
        let index = frontier.peaks.leaves;
        let mut commit = CommitRecord::new(index, base, timestamp_ms, frontier.last_commit, deltas);
        for (position, hash) in frontier.peaks.append(commit.commit_hash) {
            batch.put(keys::ledger_node(position), Bytes::copy_from_slice(&hash));
        }
        commit.merkle_root = frontier.peaks.root();
        frontier.last_commit = commit.commit_hash;
        batch.put(keys::ledger_commit(index), commit.encode());
        batch.put(
            keys::ledger_locator(base),
            Bytes::copy_from_slice(&index.to_le_bytes()),
        );
    }

    /// Run by the leader: commits every plan of `group` that still holds in one engine batch.
    fn commit_group(engine: &Db, head: &mut Head, group: &[Arc<Pending>]) {
        let finish = |results: Vec<Option<Result<Attempt>>>| {
            for (pending, result) in group.iter().zip(results) {
                pending.finish(
                    result.unwrap_or_else(|| Err(corruption("a ledger plan was not decided"))),
                );
            }
        };
        loop {
            let first = engine.last_sequence() + 1;
            let timestamp_ms = now_ms();
            let mut batch = WriteBatch::new();
            batch.at_sequence(first);
            let mut frontier = head.clone();
            let mut written: HashMap<Bytes, Option<u64>> = HashMap::new();
            let mut results: Vec<Option<Result<Attempt>>> = Vec::with_capacity(group.len());
            let mut accepted = Vec::new();
            for (i, pending) in group.iter().enumerate() {
                let plan = &pending.item;
                match Self::check(engine, plan, &written) {
                    Err(error) => results.push(Some(Err(error))),
                    Ok(Some(failed)) => results.push(Some(failed)),
                    Ok(None) => {
                        let base = first + batch.len() as u64;
                        plan.append_ops(&mut batch);
                        for (j, (key, value)) in plan.ops.iter().enumerate() {
                            written.insert(key.clone(), value.is_some().then_some(base + j as u64));
                        }
                        if !plan.deltas.is_empty() {
                            Self::append(&mut frontier, &mut batch, plan, base, timestamp_ms);
                        }
                        results.push(Some(Ok(Attempt::Committed {
                            first: base,
                            outcomes: plan.outcomes(base),
                        })));
                        accepted.push(i);
                    }
                }
            }
            if frontier != *head {
                batch.put(Bytes::from_static(keys::LEDGER_HEAD), frontier.encode());
            }
            match engine.write(batch) {
                Ok(_) => {
                    *head = frontier;
                    finish(results);
                    return;
                }
                // Another writer (an index build, a catalog change) took the sequence: decide
                // the group again against the new state.
                Err(lilia_engine::Error::SequenceMoved { .. }) => {}
                Err(error) => {
                    let error = Error::from(error);
                    for i in accepted {
                        results[i] = Some(Err(error.clone()));
                    }
                    finish(results);
                    return;
                }
            }
        }
    }

    /// The current root and number of commits.
    ///
    /// # Errors
    ///
    /// Engine errors; a head that does not decode.
    pub fn state(engine: &Db) -> Result<(Hash, u64)> {
        let head = read_head(engine, None)?;
        Ok((head.peaks.root(), head.peaks.leaves))
    }

    fn mmr_proof(engine: &Db, index: u64, head: &Head) -> Result<MmrProof> {
        let (positions, _) = proof_positions(index, head.peaks.leaves)
            .ok_or_else(|| Error::NotFound(format!("ledger commit {}", index + 1)))?;
        let path = positions
            .into_iter()
            .map(|(position, is_left)| {
                Ok(AuditHop {
                    sibling_hash: read_node(engine, position)?,
                    is_left,
                })
            })
            .collect::<Result<_>>()?;
        Ok(MmrProof {
            leaf_index: index,
            leaves: head.peaks.leaves,
            path,
            peaks: head.peaks.hashes.clone(),
        })
    }

    /// The header of commit `sequence` (1-based).
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] past the last commit; engine errors.
    pub fn commit_header(engine: &Db, sequence: u64) -> Result<LedgerCommit> {
        let head = read_head(engine, None)?;
        if sequence == 0 || sequence > head.peaks.leaves {
            return Err(Error::NotFound(format!("ledger commit {sequence}")));
        }
        Ok(read_commit(engine, sequence - 1)?.header())
    }

    /// Proof of commit `sequence` (1-based) against the current root.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] past the last commit; engine errors.
    pub fn prove_commit(engine: &Db, sequence: u64) -> Result<CommitProof> {
        let head = read_head(engine, None)?;
        if sequence == 0 || sequence > head.peaks.leaves {
            return Err(Error::NotFound(format!("ledger commit {sequence}")));
        }
        let commit = read_commit(engine, sequence - 1)?.header();
        Ok(CommitProof {
            commit,
            proof: Self::mmr_proof(engine, sequence - 1, &head)?,
            root: head.peaks.root(),
        })
    }

    /// Proof of the current version of the record stored at `key`.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] for an absent record or one written before the ledger started;
    /// [`Error::Corruption`] if the ledger does not attest the record's version.
    pub fn prove_record(engine: &Db, key: &Bytes) -> Result<RecordProof> {
        let snapshot = engine.snapshot();
        let head = read_head(engine, Some(&snapshot))?;
        let version = engine
            .get_at(&snapshot, key)?
            .ok_or_else(|| Error::NotFound(keys::describe(key)))?
            .version;
        if version < head.start_sequence {
            return Err(Error::NotFound(format!(
                "{} was written before the ledger started",
                keys::describe(key)
            )));
        }
        let (model, space, id) = record_identity(key).ok_or_else(|| invalid("not a record key"))?;
        let locator = keys::ledger_locator(version);
        let end = keys::prefix_end(keys::ledger_locator_prefix()).unwrap_or_default();
        let found = engine
            .range_at(&snapshot, Bound::Included(&locator), Bound::Excluded(&end))?
            .next()
            .transpose()?;
        let index = found
            .and_then(|entry| <[u8; 8]>::try_from(entry.value.as_ref()).ok())
            .map(u64::from_le_bytes)
            .ok_or_else(|| {
                corruption(format!("no ledger commit covers {}", keys::describe(key)))
            })?;
        let commit = read_commit(engine, index)?;
        let position = commit
            .deltas
            .iter()
            .position(|d| {
                d.version == version && d.model == model.name() && d.space == space && d.id == id
            })
            .ok_or_else(|| {
                corruption(format!(
                    "the ledger does not attest version {version} of {}",
                    keys::describe(key)
                ))
            })?;
        let tx_path = tx_tree(&commit.deltas).audit_path(position);
        Ok(RecordProof {
            delta: commit.deltas[position].clone(),
            tx_path,
            commit: commit.header(),
            proof: Self::mmr_proof(engine, index, &head)?,
            root: head.peaks.root(),
        })
    }
}
