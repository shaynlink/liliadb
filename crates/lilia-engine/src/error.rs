use std::io;
use std::sync::Arc;

/// Errors of the storage engine. Cheap to clone: one failed group commit reports the same error
/// to every writer of the group.
#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(Arc<io::Error>),

    /// Stored data failed a checksum or does not decode; the file and offset are in the message.
    #[error("corruption: {0}")]
    Corruption(String),

    /// A conditional write found a version other than the one it expected. Nothing of its batch
    /// was applied.
    #[error("version conflict on key {key:?}: expected {expected:?}, found {actual:?}")]
    Conflict { key: Vec<u8>, expected: Option<u64>, actual: Option<u64> },

    /// A batch pinned to a sequence number ([`crate::WriteBatch::at_sequence`]) would have
    /// committed at another one. Nothing of the batch was applied.
    #[error("sequence moved: the batch expected to commit at {expected}, the next sequence is {actual}")]
    SequenceMoved { expected: u64, actual: u64 },

    /// A write or sync failed earlier; the engine refuses further writes because the state of
    /// the journal on disk is unknown. Reads keep working. Reopen the database to recover.
    #[error("writes are disabled after an earlier failure: {0}")]
    Poisoned(String),

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("the database is closed")]
    Closed,
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(Arc::new(error))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Builds a [`Error::Corruption`].
pub(crate) fn corruption(what: impl Into<String>) -> Error {
    Error::Corruption(what.into())
}
