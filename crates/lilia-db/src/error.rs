//! Errors of the data-model layer.

/// Errors of [`crate::Database`]. Cheap to clone: one failed ledger group reports the same error
/// to every writer of the group.
#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    /// The storage engine failed (I/O, corruption, writes disabled after an earlier failure).
    #[error(transparent)]
    Engine(#[from] lilia_engine::Error),

    /// A conditional write (`if_version`) or a transaction found another version than the one
    /// it expected. Nothing of the batch or transaction was applied.
    #[error("version conflict on {target}: expected {expected:?}, found {actual:?}")]
    Conflict {
        target: String,
        expected: Option<u64>,
        actual: Option<u64>,
    },

    /// A row-level security policy rejects the write.
    #[error("access denied: {0}")]
    Denied(String),

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// A JSON value that cannot be stored as a document.
    #[error("invalid document: {0}")]
    Document(String),

    /// An index, filter or policy expression that does not parse.
    #[error("invalid expression `{expression}`: {reason}")]
    Expression { expression: String, reason: String },

    /// Stored data of this layer (catalog, records, ledger) that does not decode or does not
    /// match its commitments.
    #[error("corruption: {0}")]
    Corruption(String),

    #[error("not found: {0}")]
    NotFound(String),

    /// A ledger operation on a database opened without a ledger.
    #[error("the ledger is not enabled for this database")]
    LedgerDisabled,
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn corruption(what: impl Into<String>) -> Error {
    Error::Corruption(what.into())
}

pub(crate) fn invalid(what: impl Into<String>) -> Error {
    Error::InvalidArgument(what.into())
}
