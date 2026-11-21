//! Verification of the whole ledger, and of the live records against it.

use std::collections::HashMap;
use std::ops::Bound;

use lilia_core::{LedgerAction, LedgerVerificationReport, LedgerVerifyMode};
use lilia_engine::{Db, Snapshot};

use super::mmr::{Hash, Peaks};
use super::record::{CommitRecord, Head};
use super::{read_head, record_identity, Ledger};
use crate::error::Result;
use crate::keys;

/// Newest delta per record: version, action, payload hash.
type Attested = HashMap<(String, String, String), (u64, LedgerAction, Hash)>;

impl Ledger {
    /// Recomputes the ledger from its commits: hash chain, transaction roots, every node of the
    /// range and the root. [`LedgerVerifyMode::Strict`] also checks that every live record
    /// written since the ledger started is the one its newest delta attests, and that no
    /// attested record is missing.
    ///
    /// # Errors
    ///
    /// Engine errors. Integrity failures are reported in the result, not as errors.
    pub fn verify(engine: &Db, mode: LedgerVerifyMode) -> Result<LedgerVerificationReport> {
        let snapshot = engine.snapshot();
        let head = read_head(engine, Some(&snapshot))?;
        let mut report = LedgerVerificationReport {
            is_valid: true,
            commit_count: head.peaks.leaves,
            leaves_verified: 0,
            merkle_root: head.peaks.root(),
            error: None,
        };
        if mode == LedgerVerifyMode::None {
            return Ok(report);
        }
        let mut latest = Attested::new();
        let failure = Self::verify_chain(
            engine,
            &snapshot,
            &head,
            mode == LedgerVerifyMode::Strict,
            &mut latest,
        )?
        .or(if mode == LedgerVerifyMode::Strict {
            Self::verify_records(
                engine,
                &snapshot,
                &head,
                &mut latest,
                &mut report.leaves_verified,
            )?
        } else {
            None
        });
        report.is_valid = failure.is_none();
        report.error = failure;
        Ok(report)
    }

    fn verify_chain(
        engine: &Db,
        snapshot: &Snapshot,
        head: &Head,
        collect: bool,
        latest: &mut Attested,
    ) -> Result<Option<String>> {
        let mut peaks = Peaks::default();
        let mut parent = [0u8; 32];
        let end = keys::prefix_end(keys::LEDGER_COMMITS).unwrap_or_default();
        for entry in engine.range_at(
            snapshot,
            Bound::Included(keys::LEDGER_COMMITS),
            Bound::Excluded(&end),
        )? {
            let commit = CommitRecord::decode(&entry?.value)?;
            let sequence = commit.index + 1;
            if commit.index != peaks.leaves {
                return Ok(Some(format!("commit {sequence} is out of order")));
            }
            let rebuilt = CommitRecord::new(
                commit.index,
                commit.first_sequence,
                commit.timestamp_ms,
                parent,
                commit.deltas.clone(),
            );
            if commit.parent_hash != parent {
                return Ok(Some(format!(
                    "commit {sequence} does not chain to its parent"
                )));
            }
            if rebuilt.tx_root != commit.tx_root || rebuilt.commit_hash != commit.commit_hash {
                return Ok(Some(format!("commit {sequence} does not match its hash")));
            }
            for (position, hash) in peaks.append(commit.commit_hash) {
                if engine
                    .get_at(snapshot, &keys::ledger_node(position))?
                    .map(|v| v.value)
                    .as_deref()
                    != Some(&hash[..])
                {
                    return Ok(Some(format!(
                        "ledger node {position} does not match commit {sequence}"
                    )));
                }
            }
            if peaks.root() != commit.merkle_root {
                return Ok(Some(format!("commit {sequence} records another root")));
            }
            let locator = engine.get_at(snapshot, &keys::ledger_locator(commit.first_sequence))?;
            if locator.map(|v| v.value).as_deref() != Some(&commit.index.to_le_bytes()[..]) {
                return Ok(Some(format!("commit {sequence} has no locator")));
            }
            parent = commit.commit_hash;
            if collect {
                for delta in commit.deltas {
                    let newer = latest
                        .get(&(delta.model.clone(), delta.space.clone(), delta.id.clone()))
                        .is_none_or(|l| l.0 < delta.version);
                    if newer {
                        latest.insert(
                            (delta.model, delta.space, delta.id),
                            (delta.version, delta.action, delta.payload_hash),
                        );
                    }
                }
            }
        }
        if peaks != head.peaks || parent != head.last_commit {
            return Ok(Some(
                "the ledger head does not match its commits".to_owned(),
            ));
        }
        Ok(None)
    }

    fn verify_records(
        engine: &Db,
        snapshot: &Snapshot,
        head: &Head,
        latest: &mut Attested,
        verified: &mut u64,
    ) -> Result<Option<String>> {
        for tag in keys::RECORD_TAGS {
            for entry in engine.range_at(
                snapshot,
                Bound::Included(&[tag][..]),
                Bound::Excluded(&[tag + 1][..]),
            )? {
                let entry = entry?;
                if entry.version < head.start_sequence {
                    continue;
                }
                let Some((model, space, id)) = record_identity(&entry.key) else {
                    return Ok(Some(format!(
                        "unreadable record key {}",
                        hex::encode(&entry.key)
                    )));
                };
                let attested = latest.remove(&(model.name().to_owned(), space, id));
                let payload = *blake3::hash(&entry.value).as_bytes();
                if attested != Some((entry.version, LedgerAction::Put, payload)) {
                    return Ok(Some(format!(
                        "{} differs from what the ledger attests",
                        keys::describe(&entry.key)
                    )));
                }
                *verified += 1;
            }
        }
        let missing = latest
            .iter()
            .find(|(_, (_, action, _))| *action == LedgerAction::Put);
        Ok(missing.map(|((model, space, id), _)| {
            format!("{model}:{space}/{id} is attested by the ledger but missing")
        }))
    }
}
