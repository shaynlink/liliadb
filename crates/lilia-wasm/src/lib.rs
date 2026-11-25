//! `LiliaDB` in WebAssembly, for `@liliadb/wasm`: a [`Database`] runs the operations of the
//! protocol ([`lilia_protocol::Operation`], in `MessagePack`) and answers each with a JS frame
//! ([`lilia_protocol::to_js_frame`]), the same exchange as the native bindings.
//!
//! A database lives in the browser's origin private file system, on a pool of slot files
//! ([`OpfsPool`], dedicated workers only), or in memory. The engine runs without background
//! threads: the writer that fills a memtable flushes it. The crate builds for
//! `wasm32-unknown-unknown` only (`tools/wasm/build.sh`); elsewhere it is empty.

#![cfg(all(target_arch = "wasm32", target_os = "unknown"))]

use std::sync::Arc;
use std::time::Duration;

use lilia_core::{ErrorCode, LiliaError};
use lilia_db::Database as Engine;
use lilia_io::{MemVfs, OpfsPool, Vfs};
use lilia_protocol::{
    execute_js, to_js_frame, OpenOptions, RequestEncoding, ResponseValue, SharedDatabase,
};
use serde::Deserialize;
use wasm_bindgen::prelude::*;

/// The directory of a database inside its file system.
const DATABASE_PATH: &str = "/db";
/// Slots an OPFS pool starts with when the options name none.
const DEFAULT_SLOTS: usize = 64;

/// Where a database keeps its files.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Storage {
    /// The origin private file system, in directory `path`.
    #[default]
    Opfs,
    /// Memory: the database ends with its module.
    Memory,
}

/// The JSON a database opens with: where it lives, then any field of [`OpenOptions`].
#[derive(Debug, Deserialize)]
struct Open {
    /// The OPFS directory of the database (unused in memory).
    #[serde(default)]
    path: String,
    #[serde(default)]
    storage: Storage,
    /// Slots the OPFS pool starts with; it grows as the database does.
    slots: Option<usize>,
    #[serde(flatten)]
    options: OpenOptions,
}

/// `error` as a JavaScript `Error` whose message is its JSON, as the native bindings throw it.
fn thrown(error: &LiliaError) -> JsValue {
    let json = serde_json::to_string(error).unwrap_or_else(|_| error.to_string());
    js_sys::Error::new(&json).into()
}

fn failure(code: ErrorCode, message: impl Into<String>) -> JsValue {
    thrown(&LiliaError::new(code, message, false))
}

/// Reports panics on the console: a panic aborts the module, which would otherwise end silent.
#[wasm_bindgen(start)]
pub fn start() {
    std::panic::set_hook(Box::new(|info| {
        web_sys::console::error_1(&JsValue::from_str(&format!("lilia-wasm panicked: {info}")));
    }));
}

/// An open database.
#[wasm_bindgen]
#[derive(Debug)]
pub struct Database {
    database: SharedDatabase,
    /// The OPFS slot files, held open (and so locked against other tabs) while the database is.
    pool: Option<OpfsPool>,
}

#[wasm_bindgen]
impl Database {
    /// Opens (or creates) the database `options_json` describes: `{"storage": "opfs" |
    /// "memory", "path": ..., "slots": ..., "durability": ..., "profile": ..., "ledger": ...,
    /// "history_retention_ms": ..., "undo_depth": ...}`, all optional but the OPFS `path`.
    ///
    /// # Errors
    ///
    /// The JSON of the error: invalid options, OPFS out of a dedicated worker or held by
    /// another tab, the database's failure to open.
    pub async fn open(options_json: String) -> Result<Database, JsValue> {
        let open: Open = serde_json::from_str(&options_json).map_err(|e| {
            failure(
                ErrorCode::InvalidInput,
                format!("invalid open options: {e}"),
            )
        })?;
        let options = open.options.options().map_err(|e| thrown(&e))?;
        let (vfs, pool): (Arc<dyn Vfs>, _) = match open.storage {
            Storage::Memory => (Arc::new(MemVfs::new(0)), None),
            Storage::Opfs => {
                if open.path.is_empty() {
                    return Err(failure(
                        ErrorCode::InvalidInput,
                        "an OPFS database needs a path",
                    ));
                }
                let pool = OpfsPool::open(&open.path, open.slots.unwrap_or(DEFAULT_SLOTS))
                    .await
                    .map_err(|e| {
                        failure(
                            ErrorCode::Storage,
                            format!("cannot open OPFS directory {}: {e}", open.path),
                        )
                    })?;
                (Arc::new(pool.vfs()), Some(pool))
            }
        };
        let engine =
            Engine::open_with(vfs, DATABASE_PATH, options).map_err(|e| thrown(&e.into()))?;
        Ok(Self {
            database: SharedDatabase::new(engine),
            pool,
        })
    }

    /// Runs the `MessagePack` operation `request` and returns the JS frame of its result, after
    /// growing the OPFS pool when a quarter of its slots or fewer are free.
    ///
    /// # Errors
    ///
    /// The JSON of the error when the pool cannot grow or the frame exceeds 4 GiB; the
    /// operation's own failure is in the frame.
    pub async fn execute(&self, request: Vec<u8>) -> Result<Vec<u8>, JsValue> {
        if let Some(pool) = &self.pool {
            let vfs = pool.vfs();
            if vfs.free_slots() * 4 <= vfs.capacity() {
                pool.grow(vfs.capacity()).await.map_err(|e| {
                    failure(
                        ErrorCode::DiskFull,
                        format!("cannot grow the OPFS pool: {e}"),
                    )
                })?;
            }
        }
        self.execute_sync(&request)
    }

    /// Runs the `MessagePack` operation `request` in place, on the slots the pool has.
    ///
    /// # Errors
    ///
    /// The JSON of the error when the frame exceeds 4 GiB; the operation's own failure is in
    /// the frame.
    #[wasm_bindgen(js_name = executeSync)]
    pub fn execute_sync(&self, request: &[u8]) -> Result<Vec<u8>, JsValue> {
        let frame = match self.database.get() {
            Ok(database) => execute_js(&database, request, RequestEncoding::MessagePack),
            Err(error) => to_js_frame(&Err::<ResponseValue, _>(error)),
        };
        frame.map_err(|e| failure(ErrorCode::InvalidInput, e.to_string()))
    }

    /// Closes the database, making every acknowledged write durable, and releases its files;
    /// later calls fail with `CLOSED`.
    ///
    /// # Errors
    ///
    /// The JSON of the error the close met.
    pub fn close(&mut self) -> Result<(), JsValue> {
        // A single-threaded module: no call holds the database while this runs.
        let closed = self.database.close(Duration::ZERO).map_err(|e| thrown(&e));
        self.pool = None;
        closed
    }
}
