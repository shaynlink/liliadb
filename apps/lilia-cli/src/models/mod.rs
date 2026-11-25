//! The commands that open the database. Those the protocol has run through its executor
//! ([`operations`]), as the daemon and the bindings run them; bitemporal reads and the ledger
//! use the database itself.

mod bitemporal;
pub(crate) mod input;
mod ledger;
pub(crate) mod operations;

use std::path::Path;

use lilia_core::{ErrorCode, LiliaError};
use lilia_db::Database;
use serde::Serialize;
use serde_json::{json, Value};

use crate::args::{DatabaseCommand, OpenArgs};

/// `value` as JSON.
pub(crate) fn render(value: &impl Serialize) -> Result<Value, LiliaError> {
    serde_json::to_value(value).map_err(|e| {
        LiliaError::new(
            ErrorCode::Storage,
            format!("cannot render the result: {e}"),
            false,
        )
    })
}

/// Opens the database of `path` with `open`.
pub(crate) fn open(path: &Path, open: &OpenArgs) -> Result<Database, LiliaError> {
    lilia_protocol::open(path, &open.options())
}

/// Runs `command` on the database of `path`, then closes it.
pub(crate) fn execute(
    command: DatabaseCommand,
    path: &Path,
    options: &OpenArgs,
) -> anyhow::Result<Value> {
    let database = open(path, options)?;
    let result = run(&database, path, command);
    // The close makes every acknowledged write durable: its failure is the command's.
    database.close().map_err(LiliaError::from)?;
    result
}

fn run(database: &Database, path: &Path, command: DatabaseCommand) -> anyhow::Result<Value> {
    let call = match command {
        DatabaseCommand::Init => return Ok(json!({"ok": true, "path": path})),
        DatabaseCommand::Doctor { deep } => return doctor(database, path, deep),
        DatabaseCommand::Bitemp { command } => return Ok(bitemporal::run(database, command)?),
        DatabaseCommand::Ledger { command } => return ledger::run(database, command),
        DatabaseCommand::Kv { command } => operations::kv(command)?,
        DatabaseCommand::Json { command } => operations::json(command)?,
        DatabaseCommand::Batch { file } => operations::batch(&file)?,
        DatabaseCommand::Sql { query, params } => operations::sql(query, &params),
        DatabaseCommand::Graph { command } => operations::graph(command)?,
        DatabaseCommand::Fts { command } => operations::fts(command),
        DatabaseCommand::Vector { command } => operations::vector(command)?,
        DatabaseCommand::Hybrid { command } => operations::hybrid(command)?,
        DatabaseCommand::Crdt { command } => operations::crdt(command)?,
    };
    Ok(operations::run(database, call)?)
}

/// The integrity of the database (every entry read back when `deep`) and its storage.
fn doctor(database: &Database, path: &Path, deep: bool) -> anyhow::Result<Value> {
    let report = database.check_integrity(!deep).map_err(LiliaError::from)?;
    Ok(json!({
        "path": path,
        "deep": deep,
        "integrity": render(&report)?,
        "storage": render(&database.storage_stats())?,
    }))
}
