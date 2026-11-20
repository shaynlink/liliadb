//! `LiliaDB` data models over the native storage engine ([`lilia_engine`]).
//!
//! - **Key-value**: namespaced byte keys and values, with optional expiry
//!   ([`Session::kv_get`], [`Session::kv_scan`]).
//! - **JSON documents**: documents by id in spaces, stored in the binary document format, with
//!   secondary indexes over paths or expressions, partial indexes, and equality queries served
//!   by the indexes ([`Session::json_find`], [`Database::create_index`]).
//! - **Conditional batches**: atomic groups of writes over every model, each optionally
//!   conditional on the version of its record ([`Batch`], [`Write`]). A record's version is the
//!   engine sequence number of the write that produced it.
//! - **Transactions**: optimistic, over a snapshot, validated at commit ([`Transaction`]).
//! - **Row-level security and masking**: every read and write of a [`Session`] passes through
//!   one guard that applies the policies and masking rules of the collection for the caller's
//!   [`SecurityContext`] ([`guard`]).
//! - **Ledger**: an optional tamper-evident history of every record write, a Merkle Mountain
//!   Range with `O(log n)` appends and proofs ([`ledger`]).

mod batch;
mod catalog;
mod db;
mod error;
pub mod find;
pub mod guard;
mod keys;
pub mod ledger;
mod read;
mod record;
mod session;
mod txn;
mod write;

pub use batch::{Batch, Outcome, Write};
pub use db::{Database, Options};
pub use error::{Error, Result};
pub use guard::Access;
pub use ledger::mmr::MmrProof;
pub use ledger::{CommitProof, RecordProof};
pub use lilia_core::{
    BinaryDoc, IndexOptions, JsonEntry, JsonIndexInfo, KvEntry, LedgerCommit,
    LedgerVerificationReport, LedgerVerifyMode, MaskFunction, MaskingRule, NamedPolicy, Policy,
    PolicyAction, SecurityContext,
};
pub use read::Scan;
pub use session::Session;
pub use txn::Transaction;
