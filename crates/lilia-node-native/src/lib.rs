//! The Node.js binding of `LiliaDB`: a [`NativeDatabase`] that runs the operations of
//! [`lilia_protocol`] through its system session. An operation goes in, and its result comes
//! out, as `MessagePack`, the same payloads the daemon exchanges with Node clients, so the
//! TypeScript package has one client for both. [`NativeDatabase::execute`] runs on a blocking
//! worker, [`NativeDatabase::execute_sync`] on the caller's thread.

// reason: N-API functions take their arguments owned, as JavaScript passes them, and their
// errors are JavaScript exceptions, documented by the TypeScript package.
#![allow(clippy::needless_pass_by_value, clippy::missing_errors_doc)]

use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use lilia_core::{ErrorCode, LiliaError};
use lilia_db::Database;
use lilia_protocol::{from_msgpack, to_msgpack, OpenOptions, Operation, ResponseValue};
use napi::bindgen_prelude::{Buffer, Error, Result, Status};
use napi_derive::napi;

/// An open database. Calls share it; [`NativeDatabase::close`] closes it once they return.
#[napi]
#[derive(Debug)]
pub struct NativeDatabase {
    inner: RwLock<Option<Arc<Database>>>,
}

#[napi(object)]
#[derive(Debug)]
pub struct NativeOpenOptions {
    /// Directory of the database; created if missing.
    pub path: String,
    /// `durable` (default), `balanced` or `performance`.
    pub durability: Option<String>,
    /// `server` (default), `mobile` or `constrained`.
    pub profile: Option<String>,
    /// Keep a tamper-evident ledger of every record write.
    pub ledger: Option<bool>,
    /// How long old versions stay readable, in milliseconds; a week by default.
    pub history_retention_ms: Option<i64>,
    /// Commits that can be undone; none by default.
    pub undo_depth: Option<u32>,
}

/// `error` as a JavaScript error whose message is the JSON of the error; retryable errors
/// carry the `WouldDeadlock` status.
fn napi_error(error: LiliaError) -> Error {
    Error::new(
        if error.retryable {
            Status::WouldDeadlock
        } else {
            Status::GenericFailure
        },
        serde_json::to_string(&error).unwrap_or_else(|_| error.to_string()),
    )
}

fn invalid_arg(message: impl Into<String>) -> Error {
    napi_error(LiliaError::new(ErrorCode::InvalidInput, message, false))
}

fn join_error(error: tokio::task::JoinError) -> Error {
    napi_error(LiliaError::new(
        ErrorCode::Storage,
        format!("worker failed: {error}"),
        false,
    ))
}

/// Runs the operation `payload` encodes; the `MessagePack` of its result, `{ Ok: value }` or
/// `{ Err: error }`, a payload that does not decode included.
fn run(database: &Database, payload: &[u8]) -> Result<Buffer> {
    let result: std::result::Result<ResponseValue, LiliaError> = from_msgpack::<Operation>(payload)
        .map_err(|e| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                format!("invalid operation: {e}"),
                false,
            )
        })
        .and_then(|operation| lilia_protocol::execute(database, operation));
    to_msgpack(&result)
        .map(Buffer::from)
        .map_err(|e| napi_error(LiliaError::new(ErrorCode::Storage, e.to_string(), false)))
}

impl NativeDatabase {
    /// The open database, or a `CLOSED` error.
    fn database(&self) -> Result<Arc<Database>> {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| {
                napi_error(LiliaError::new(
                    ErrorCode::Closed,
                    "the database is closed",
                    false,
                ))
            })
    }
}

#[napi]
impl NativeDatabase {
    #[napi(factory)]
    pub async fn open(options: NativeOpenOptions) -> Result<Self> {
        let history_retention_ms = options
            .history_retention_ms
            .map(|ms| u64::try_from(ms).map_err(|_| invalid_arg("historyRetentionMs must be >= 0")))
            .transpose()?;
        let open = OpenOptions {
            durability: options.durability,
            profile: options.profile,
            ledger: options.ledger.unwrap_or(false),
            history_retention_ms,
            undo_depth: options.undo_depth.unwrap_or(0),
        };
        let path = options.path;
        let database = tokio::task::spawn_blocking(move || lilia_protocol::open(path, &open))
            .await
            .map_err(join_error)?
            .map_err(napi_error)?;
        Ok(Self {
            inner: RwLock::new(Some(Arc::new(database))),
        })
    }

    /// Closes the database once the calls running on it return, waiting at most `timeout_ms`
    /// (5 seconds by default). Later calls fail with `CLOSED`; closing twice does nothing.
    #[napi]
    pub async fn close(&self, timeout_ms: Option<u32>) -> Result<()> {
        let taken = self
            .inner
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(database) = taken else {
            return Ok(());
        };
        let timeout = Duration::from_millis(u64::from(timeout_ms.unwrap_or(5_000)));
        tokio::task::spawn_blocking(move || lilia_protocol::close(database, timeout))
            .await
            .map_err(join_error)?
            .map_err(napi_error)
    }

    /// Runs the operation `payload` encodes on a blocking worker; see [`run`].
    #[napi]
    pub async fn execute(&self, payload: Buffer) -> Result<Buffer> {
        let database = self.database()?;
        tokio::task::spawn_blocking(move || run(&database, &payload))
            .await
            .map_err(join_error)?
    }

    /// Runs the operation `payload` encodes on the caller's thread; see [`run`].
    #[napi]
    pub fn execute_sync(&self, payload: Buffer) -> Result<Buffer> {
        let database = self.database()?;
        run(&database, &payload)
    }
}
