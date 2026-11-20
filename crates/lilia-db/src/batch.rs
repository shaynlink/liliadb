//! Atomic groups of writes over every model.

use lilia_core::BinaryDoc;

use crate::graph::GraphWrite;
use serde_json::Value;

use crate::error::{Error, Result};

/// One write of a [`Batch`].
///
/// `if_version` makes the write conditional: `Some(0)` requires the record to be absent (an
/// expired key-value record counts as absent), `Some(v)` requires its current version to be
/// `v`. A failed condition fails the whole batch with [`Error::Conflict`].
#[derive(Debug, Clone, PartialEq)]
pub enum Write {
    KvPut {
        namespace: String,
        key: Vec<u8>,
        value: Vec<u8>,
        expires_at_ms: Option<i64>,
        if_version: Option<u64>,
    },
    KvDelete {
        namespace: String,
        key: Vec<u8>,
        if_version: Option<u64>,
    },
    JsonPut {
        space: String,
        id: String,
        doc: BinaryDoc,
        if_version: Option<u64>,
    },
    JsonDelete {
        space: String,
        id: String,
        if_version: Option<u64>,
    },
    Graph(GraphWrite),
}

/// Writes applied atomically, all of them or none, in order: a later write of a record sees the
/// earlier ones.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Batch {
    writes: Vec<Write>,
}

impl Batch {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, write: Write) -> &mut Self {
        self.writes.push(write);
        self
    }

    pub fn kv_put(
        &mut self,
        namespace: &str,
        key: impl Into<Vec<u8>>,
        value: impl Into<Vec<u8>>,
    ) -> &mut Self {
        self.push(Write::KvPut {
            namespace: namespace.to_owned(),
            key: key.into(),
            value: value.into(),
            expires_at_ms: None,
            if_version: None,
        })
    }

    pub fn kv_delete(&mut self, namespace: &str, key: impl Into<Vec<u8>>) -> &mut Self {
        self.push(Write::KvDelete {
            namespace: namespace.to_owned(),
            key: key.into(),
            if_version: None,
        })
    }

    /// Adds a document write.
    ///
    /// # Errors
    ///
    /// [`Error::Document`] when `value` cannot be stored as a document.
    pub fn json_put(&mut self, space: &str, id: &str, value: &Value) -> Result<&mut Self> {
        let doc = BinaryDoc::from_json_value(value).map_err(|e| Error::Document(e.to_string()))?;
        Ok(self.push(Write::JsonPut {
            space: space.to_owned(),
            id: id.to_owned(),
            doc,
            if_version: None,
        }))
    }

    pub fn json_delete(&mut self, space: &str, id: &str) -> &mut Self {
        self.push(Write::JsonDelete {
            space: space.to_owned(),
            id: id.to_owned(),
            if_version: None,
        })
    }

    #[must_use]
    pub fn writes(&self) -> &[Write] {
        &self.writes
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.writes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.writes.is_empty()
    }
}

impl From<Vec<Write>> for Batch {
    fn from(writes: Vec<Write>) -> Self {
        Self { writes }
    }
}

/// What one write of a batch did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    /// Version of the record written (for a delete, of its tombstone); `None` when the write
    /// changed nothing (a delete of an absent record).
    pub version: Option<u64>,
    /// Whether a delete removed a live record.
    pub deleted: bool,
}
