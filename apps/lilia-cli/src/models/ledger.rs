//! The ledger: its root, its commits, proofs that a record or a commit is in it, and their
//! verification.

use std::fs;
use std::path::Path;

use lilia_core::{LedgerVerifyMode, LiliaError};
use lilia_db::{CommitProof, Database, RecordProof};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{input, render};
use crate::args::models::LedgerCommand;

/// A proof file, as `prove-doc`, `prove-kv` or `prove-commit` write it.
#[derive(Deserialize)]
#[serde(untagged)]
enum Proof {
    Record(RecordProof),
    Commit(CommitProof),
}

pub(crate) fn run(database: &Database, command: LedgerCommand) -> anyhow::Result<Value> {
    Ok(match command {
        LedgerCommand::Root => json!({
            "merkle_root": hex::encode(database.ledger_root().map_err(LiliaError::from)?),
            "commits": database.ledger_len().map_err(LiliaError::from)?,
        }),
        LedgerCommand::Latest => match database.ledger_len().map_err(LiliaError::from)? {
            0 => Value::Null,
            last => render(&database.ledger_commit(last).map_err(LiliaError::from)?)?,
        },
        LedgerCommand::ProveDoc { space, id } => {
            render(&database.prove_doc(&space, &id).map_err(LiliaError::from)?)?
        }
        LedgerCommand::ProveKv {
            namespace,
            key,
            encoding,
        } => {
            let key = input::bytes(&key, encoding, "key")?;
            render(
                &database
                    .prove_kv(&namespace, &key)
                    .map_err(LiliaError::from)?,
            )?
        }
        LedgerCommand::ProveCommit { sequence } => {
            render(&database.prove_commit(sequence).map_err(LiliaError::from)?)?
        }
        LedgerCommand::VerifyProof {
            proof_file,
            expected_root,
        } => verify(&proof_file, expected_root.as_deref())?,
        LedgerCommand::VerifyDb => render(
            &database
                .verify_ledger(LedgerVerifyMode::Strict)
                .map_err(LiliaError::from)?,
        )?,
    })
}

/// Verifies the proof of `file` against `expected_root` (hex), else against the root it was
/// made against: only a root obtained from a trusted source makes the verification mean that
/// the record is in that ledger.
fn verify(file: &Path, expected_root: Option<&str>) -> anyhow::Result<Value> {
    let proof: Proof = serde_json::from_slice(&fs::read(file)?)?;
    let (stated, commit) = match &proof {
        Proof::Record(record) => (record.root, &record.commit),
        Proof::Commit(commit) => (commit.root, &commit.commit),
    };
    let root = match expected_root {
        Some(hex_root) => {
            let mut root = [0u8; 32];
            hex::decode_to_slice(hex_root.trim_start_matches("0x"), &mut root)
                .map_err(|e| input::invalid(format!("invalid expected root: {e}")))?;
            root
        }
        None => stated,
    };
    let valid = match &proof {
        Proof::Record(record) => record.verify(&root),
        Proof::Commit(commit) => commit.verify(&root),
    };
    Ok(json!({
        "valid": valid,
        "merkle_root": hex::encode(root),
        "root_source": if expected_root.is_some() { "expected" } else { "proof" },
        "commit_seq": commit.commit_seq,
        "timestamp_ms": commit.timestamp_ms,
    }))
}
