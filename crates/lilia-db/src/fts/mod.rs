//! Full-text search: an inverted index per collection, BM25 ranking, snippets, suggestions,
//! fuzzy and phonetic search.
//!
//! Keyspaces: the indexed document (`collection\0doc_id`: its text and terms; the record the
//! ledger attests), postings (`collection\0term\0doc_id`: positions and document length, so
//! ranking reads no document), sharded document and token counts, and dictionary words.

mod handle;
mod search;
mod store;
mod text;
mod vocab;

use bytes::Bytes;

pub use handle::Fts;
pub(crate) use store::doc_key;
pub(crate) use vocab::{Deltas, Vocabularies};

use crate::error::{corruption, Result};
use crate::keys::validate_name;
use crate::txn::View;
use crate::write::overlay::Changes;
use crate::write::{Model, Planner};

/// A full-text write, for a [`crate::Batch`]; see [`Fts`] for each operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FtsWrite {
    Index {
        collection: String,
        doc_id: String,
        content: String,
    },
    Delete {
        collection: String,
        doc_id: String,
    },
    LoadDictionary {
        collection: String,
        words: Vec<String>,
    },
}

impl FtsWrite {
    fn collection(&self) -> &str {
        match self {
            Self::Index { collection, .. }
            | Self::Delete { collection, .. }
            | Self::LoadDictionary { collection, .. } => collection,
        }
    }

    fn validate(&self) -> Result<()> {
        validate_name("collection", self.collection())?;
        match self {
            Self::Index { doc_id, .. } | Self::Delete { doc_id, .. } => {
                validate_name("document id", doc_id)
            }
            Self::LoadDictionary { .. } => Ok(()),
        }
    }

    fn effect(&self, read: &mut store::Read<'_>) -> Result<store::Effect> {
        match self {
            Self::Index {
                collection,
                doc_id,
                content,
            } => store::index(read, collection, doc_id, content),
            Self::Delete { collection, doc_id } => store::delete(read, collection, doc_id),
            Self::LoadDictionary { collection, words } => {
                store::dictionary(read, collection, words)
            }
        }
    }
}

/// Ledger identity of the record at `rest` (a key after `collection\0`) of keyspace `tag`.
pub(crate) fn identity(tag: u8, rest: &[u8]) -> Option<(Model, String)> {
    let id = String::from_utf8(rest.to_vec()).ok()?;
    match tag {
        crate::keys::FTS_DOC => Some((Model::Fts, id)),
        crate::keys::FTS_DICTIONARY => Some((Model::FtsDictionary, id)),
        _ => None,
    }
}

impl Planner<'_> {
    /// Plans one full-text write.
    ///
    /// # Errors
    ///
    /// Invalid names, records that do not decode, engine read errors.
    pub(crate) fn fts(&mut self, write: &FtsWrite) -> Result<()> {
        write.validate()?;
        let effect = write.effect(&mut |key: &Bytes| self.current(key))?;
        let collection = write.collection();
        let mut record_op = None;
        for change in effect.changes {
            if change.record {
                let tag = change.key.first().copied().unwrap_or_default();
                let rest =
                    crate::keys::suffix(&change.key, &crate::keys::collection(tag, collection))
                        .to_vec();
                let (model, id) = identity(tag, &rest)
                    .ok_or_else(|| corruption("unreadable full-text record key"))?;
                record_op =
                    Some(self.write_record(&change.key, change.value, (model, collection, id)));
            } else {
                self.op(change.key, change.value);
            }
        }
        self.vocabulary(collection, effect.vocabulary);
        self.result(record_op, effect.deleted);
        Ok(())
    }
}

/// The records a full-text write changes in a transaction's view; see
/// [`crate::write::overlay`].
///
/// # Errors
///
/// Invalid names, records that do not decode, engine read errors.
pub(crate) fn overlay(write: &FtsWrite, view: &View<'_>) -> Result<(Changes, bool)> {
    write.validate()?;
    let effect = write.effect(&mut |key: &Bytes| Ok(view.peek(key)?.map(|item| item.value)))?;
    Ok((
        effect
            .changes
            .into_iter()
            .map(|c| (c.key, c.value))
            .collect(),
        effect.deleted,
    ))
}
