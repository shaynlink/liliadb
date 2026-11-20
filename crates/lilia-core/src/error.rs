use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, Error, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug))]
#[error("{message}")]
pub struct LiliaError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[rkyv(with = rkyv::with::Map<crate::zero_copy::AsJsonString>)]
    pub details: Option<Value>,
    pub request_id: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq, Eq))]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    Closed,
    Busy,
    Conflict,
    Corrupt,
    DiskFull,
    InvalidInput,
    Io,
    NotFound,
    Storage,
    Timeout,
    Unauthorized,
    PluginIncompatible,
    PluginMigrationFailed,
    Unsupported,
    /// A declarative integrity contract (CHECK, reference, restrict) rejected the write.
    ConstraintViolation,
    /// Cryptographic ledger verification failed: data or ledger tampering detected.
    TamperDetected,
}

/// Category of integrity contract reported by [`LiliaError::constraint_violation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintKind {
    /// A JSON document failed its collection `CHECK` expression.
    Check,
    /// A child row points to a parent document that does not exist.
    Reference,
    /// A parent document cannot be deleted while children still reference it.
    Restrict,
}

impl LiliaError {
    /// Builds a non-retryable `CONSTRAINT_VIOLATION` error with structured `details`.
    ///
    /// `details` is merged with `{ "constraint": name, "kind": kind }`; it must be a JSON object.
    pub fn constraint_violation(
        kind: ConstraintKind,
        constraint: &str,
        message: impl Into<String>,
        details: Value,
    ) -> Self {
        let mut object = match details {
            Value::Object(map) => map,
            other => {
                let mut map = serde_json::Map::new();
                map.insert("context".to_owned(), other);
                map
            }
        };
        object.insert(
            "constraint".to_owned(),
            Value::String(constraint.to_owned()),
        );
        object.insert(
            "kind".to_owned(),
            serde_json::to_value(kind).unwrap_or(Value::Null),
        );
        Self::new(ErrorCode::ConstraintViolation, message, false).details(Value::Object(object))
    }

    pub fn new(code: ErrorCode, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
            details: None,
            request_id: Uuid::new_v4(),
        }
    }

    #[must_use]
    pub fn with_request_id(mut self, request_id: Uuid) -> Self {
        self.request_id = request_id;
        self
    }

    #[must_use]
    pub fn details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

pub type Result<T> = std::result::Result<T, LiliaError>;
