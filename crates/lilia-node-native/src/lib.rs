//! The Node.js binding of `LiliaDB`: a [`NativeDatabase`] that runs the operations of
//! [`lilia_protocol`] through its system session. An operation comes in as `MessagePack`; its
//! result goes out as a JS frame ([`lilia_protocol::to_js_frame`]): one JSON text for V8's
//! native `JSON.parse`, then the raw bytes it refers to. The daemon answers its JavaScript
//! clients with the same frames, so the TypeScript package has one client for both.
//!
//! [`NativeDatabase::execute_sync`] runs on the caller's thread and writes into a buffer the
//! caller reuses from call to call; [`NativeDatabase::execute`] runs on the libuv thread pool.

// reason: N-API functions take their arguments owned, as JavaScript passes them, and their
// errors are JavaScript exceptions, documented by the TypeScript package.
#![allow(clippy::needless_pass_by_value, clippy::missing_errors_doc)]

use std::sync::Arc;
use std::time::Duration;

use lilia_core::{ErrorCode, LiliaError};
use lilia_db::Database;
use lilia_protocol::{execute_js, OpenOptions, RequestEncoding, SharedDatabase};
use napi::bindgen_prelude::{AsyncTask, Buffer, Either, Env, Error, Result, Status, Task};
use napi_derive::napi;

/// An open database. Calls share it; [`NativeDatabase::close`] closes it once they return.
#[napi]
#[derive(Debug)]
pub struct NativeDatabase {
    database: SharedDatabase,
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

/// Runs the operation `request` encodes (in `MessagePack`); the JS frame of its result.
fn run(database: &Database, request: &[u8]) -> Result<Vec<u8>> {
    execute_js(database, request, RequestEncoding::MessagePack)
        .map_err(|e| napi_error(LiliaError::new(ErrorCode::Storage, e.to_string(), false)))
}

/// Opens a database on the libuv pool.
#[derive(Debug)]
pub struct Open {
    path: String,
    options: OpenOptions,
}

impl Task for Open {
    type Output = Database;
    type JsValue = NativeDatabase;

    fn compute(&mut self) -> Result<Database> {
        lilia_protocol::open(&self.path, &self.options).map_err(napi_error)
    }

    fn resolve(&mut self, _env: Env, database: Database) -> Result<NativeDatabase> {
        Ok(NativeDatabase {
            database: SharedDatabase::new(database),
        })
    }
}

/// Runs an operation on the libuv pool.
#[derive(Debug)]
pub struct Execute {
    database: Arc<Database>,
    request: Vec<u8>,
}

impl Task for Execute {
    type Output = Vec<u8>;
    type JsValue = Buffer;

    fn compute(&mut self) -> Result<Vec<u8>> {
        run(&self.database, &self.request)
    }

    fn resolve(&mut self, _env: Env, frame: Vec<u8>) -> Result<Buffer> {
        Ok(Buffer::from(frame))
    }
}

/// Closes a database on the libuv pool.
#[derive(Debug)]
pub struct Close {
    database: Option<Arc<Database>>,
    timeout: Duration,
}

impl Task for Close {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> Result<()> {
        match self.database.take() {
            Some(database) => lilia_protocol::close(database, self.timeout).map_err(napi_error),
            None => Ok(()),
        }
    }

    fn resolve(&mut self, _env: Env, (): ()) -> Result<()> {
        Ok(())
    }
}

impl NativeDatabase {
    /// The open database, or a `CLOSED` error.
    fn database(&self) -> Result<Arc<Database>> {
        self.database.get().map_err(napi_error)
    }
}

/// Opens (or creates) the database `options` describe.
#[napi]
pub fn open(options: NativeOpenOptions) -> Result<AsyncTask<Open>> {
    let history_retention_ms = options
        .history_retention_ms
        .map(|ms| u64::try_from(ms).map_err(|_| invalid_arg("historyRetentionMs must be >= 0")))
        .transpose()?;
    Ok(AsyncTask::new(Open {
        path: options.path,
        options: OpenOptions {
            durability: options.durability,
            profile: options.profile,
            ledger: options.ledger.unwrap_or(false),
            history_retention_ms,
            undo_depth: options.undo_depth.unwrap_or(0),
        },
    }))
}

#[napi]
impl NativeDatabase {
    /// Closes the database once the calls running on it return, waiting at most `timeout_ms`
    /// (5 seconds by default). Later calls fail with `CLOSED`; closing twice does nothing.
    #[napi]
    pub fn close(&self, timeout_ms: Option<u32>) -> AsyncTask<Close> {
        let database = self.database.take();
        AsyncTask::new(Close {
            database,
            timeout: Duration::from_millis(u64::from(timeout_ms.unwrap_or(5_000))),
        })
    }

    /// Runs the operation `request` encodes on the libuv pool; the JS frame of its result.
    #[napi]
    pub fn execute(&self, request: &[u8]) -> Result<AsyncTask<Execute>> {
        Ok(AsyncTask::new(Execute {
            database: self.database()?,
            request: request.to_vec(),
        }))
    }

    /// Runs the operation `request` encodes on the caller's thread and writes the JS frame of
    /// its result into `out`: its length when it fits, else the frame in a new buffer.
    #[napi]
    pub fn execute_sync(&self, request: &[u8], mut out: Buffer) -> Result<Either<u32, Buffer>> {
        let database = self.database()?;
        let frame = run(&database, request)?;
        let target: &mut [u8] = out.as_mut();
        if frame.len() > target.len() {
            return Ok(Either::B(Buffer::from(frame)));
        }
        target[..frame.len()].copy_from_slice(&frame);
        u32::try_from(frame.len())
            .map(Either::A)
            .map_err(|_| invalid_arg("the frame passes 4 GiB"))
    }
}
