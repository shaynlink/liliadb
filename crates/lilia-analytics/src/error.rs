use datafusion::arrow::error::ArrowError;
use datafusion::error::DataFusionError;
use lilia_core::{ErrorCode, LiliaError};

/// Failure raised by the vectorized analytics engine.
#[derive(Debug, thiserror::Error)]
pub enum AnalyticsError {
    /// The caller supplied a schema, identifier or option the engine cannot accept.
    #[error("invalid analytics input: {0}")]
    InvalidInput(String),
    /// A persisted columnar segment could not be decoded.
    #[error("corrupt columnar segment: {0}")]
    Corrupt(String),
    /// An Arrow kernel or IPC codec failed.
    #[error("arrow error: {0}")]
    Arrow(#[from] Box<ArrowError>),
    /// `DataFusion` rejected or failed to execute the query.
    #[error("analytics query failed: {0}")]
    Query(#[from] Box<DataFusionError>),
    /// The dedicated analytics runtime could not be started or stopped answering.
    #[error("analytics runtime error: {0}")]
    Runtime(String),
    /// The row store backing a columnar table failed.
    #[error(transparent)]
    Storage(#[from] Box<LiliaError>),
}

/// Result alias used across the analytics engine.
pub type Result<T> = std::result::Result<T, AnalyticsError>;

impl From<ArrowError> for AnalyticsError {
    fn from(error: ArrowError) -> Self {
        Self::Arrow(Box::new(error))
    }
}

impl From<DataFusionError> for AnalyticsError {
    fn from(error: DataFusionError) -> Self {
        Self::Query(Box::new(error))
    }
}

impl From<LiliaError> for AnalyticsError {
    fn from(error: LiliaError) -> Self {
        Self::Storage(Box::new(error))
    }
}

impl From<AnalyticsError> for LiliaError {
    fn from(error: AnalyticsError) -> Self {
        let code = match &error {
            AnalyticsError::Storage(inner) => return (**inner).clone(),
            AnalyticsError::InvalidInput(_) => ErrorCode::InvalidInput,
            AnalyticsError::Corrupt(_) => ErrorCode::Corrupt,
            AnalyticsError::Query(inner) => query_error_code(inner),
            AnalyticsError::Arrow(_) | AnalyticsError::Runtime(_) => ErrorCode::Storage,
        };
        Self::new(code, error.to_string(), false)
    }
}

/// Maps a `DataFusion` failure onto the closest `LiliaDB` error code.
///
/// A storage failure raised while scanning travels through `DataFusion` as an external
/// error; its original code is preserved so callers still see `Busy`, `Closed`, etc.
fn query_error_code(error: &DataFusionError) -> ErrorCode {
    match error.find_root() {
        DataFusionError::Plan(_) | DataFusionError::SQL(..) | DataFusionError::SchemaError(..) => {
            ErrorCode::InvalidInput
        }
        DataFusionError::NotImplemented(_) => ErrorCode::Unsupported,
        DataFusionError::External(source) => match source.downcast_ref::<AnalyticsError>() {
            Some(AnalyticsError::Storage(inner)) => inner.code,
            Some(AnalyticsError::InvalidInput(_)) => ErrorCode::InvalidInput,
            Some(AnalyticsError::Corrupt(_)) => ErrorCode::Corrupt,
            _ => ErrorCode::Storage,
        },
        _ => ErrorCode::Storage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_errors_are_reported_as_invalid_input() {
        let error = AnalyticsError::from(DataFusionError::Plan("unknown column".into()));
        assert_eq!(LiliaError::from(error).code, ErrorCode::InvalidInput);
    }

    #[test]
    fn storage_errors_keep_their_original_code() {
        let busy = LiliaError::new(ErrorCode::Busy, "writer queue full", true);
        let wrapped = DataFusionError::External(Box::new(AnalyticsError::from(busy.clone())));
        assert_eq!(
            LiliaError::from(AnalyticsError::from(wrapped)).code,
            ErrorCode::Busy
        );
        let direct = LiliaError::from(AnalyticsError::from(busy));
        assert_eq!(direct.code, ErrorCode::Busy);
        assert!(direct.retryable);
    }
}
