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

    /// A read in the past, before the history kept ([`crate::Options::history_retention`]).
    #[error("history unavailable: {0}")]
    HistoryUnavailable(String),

    /// An operation of the v0.1 API that v0.2 does not have.
    #[error("unsupported: {0}")]
    Unsupported(String),

    /// A write through a session in the past ([`crate::Session::at_version`]).
    #[error("a session in the past is read-only")]
    ReadOnly,

    /// A SQL query that does not parse, names unknown columns, writes, or fails to run.
    #[error("SQL: {0}")]
    Sql(String),

    /// A ledger operation on a database opened without a ledger.
    #[error("the ledger is not enabled for this database")]
    LedgerDisabled,
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<Error> for lilia_core::LiliaError {
    /// The error code each error carries across the bindings; conflicts are retryable.
    fn from(error: Error) -> Self {
        use lilia_core::ErrorCode;
        let code = match &error {
            Error::Engine(engine) => match engine {
                lilia_engine::Error::Io(io) if io.kind() == std::io::ErrorKind::StorageFull => {
                    ErrorCode::DiskFull
                }
                lilia_engine::Error::Io(_) => ErrorCode::Io,
                lilia_engine::Error::Corruption(_) => ErrorCode::Corrupt,
                lilia_engine::Error::Conflict { .. }
                | lilia_engine::Error::SequenceMoved { .. } => ErrorCode::Conflict,
                lilia_engine::Error::Poisoned(_) => ErrorCode::Storage,
                lilia_engine::Error::HistoryUnavailable { .. } => ErrorCode::NotFound,
                lilia_engine::Error::InvalidArgument(_) => ErrorCode::InvalidInput,
                lilia_engine::Error::Closed => ErrorCode::Closed,
            },
            Error::Conflict { .. } => ErrorCode::Conflict,
            Error::Denied(_) => ErrorCode::Unauthorized,
            Error::InvalidArgument(_)
            | Error::Document(_)
            | Error::Expression { .. }
            | Error::Sql(_)
            | Error::ReadOnly => ErrorCode::InvalidInput,
            Error::Corruption(_) => ErrorCode::Corrupt,
            Error::NotFound(_) | Error::HistoryUnavailable(_) => ErrorCode::NotFound,
            Error::LedgerDisabled | Error::Unsupported(_) => ErrorCode::Unsupported,
        };
        Self::new(code, error.to_string(), code == ErrorCode::Conflict)
    }
}

#[cfg(feature = "sql")]
impl From<lilia_analytics::AnalyticsError> for Error {
    fn from(error: lilia_analytics::AnalyticsError) -> Self {
        Self::Sql(error.to_string())
    }
}

pub(crate) fn corruption(what: impl Into<String>) -> Error {
    Error::Corruption(what.into())
}

pub(crate) fn invalid(what: impl Into<String>) -> Error {
    Error::InvalidArgument(what.into())
}
