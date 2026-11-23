//! Operations run on a database through `execute`, the path every binding takes.

use lilia_core::BatchOperation;
use lilia_db::{Database, Durability, Options, ResourceProfile};
use lilia_protocol::{execute, Operation, ResponseValue};

mod analyses;
mod documents;
mod models;

fn open(dir: &std::path::Path) -> Database {
    let mut options = Options::for_profile(ResourceProfile::Constrained);
    options.engine.durability = Durability::Performance;
    options.undo_depth = 8;
    Database::open(dir, options).unwrap()
}

fn run(db: &Database, operation: Operation) -> ResponseValue {
    execute(db, operation).unwrap()
}

fn floats(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn batch(db: &Database, operations: Vec<BatchOperation>) -> Vec<u64> {
    let ResponseValue::Mutations(results) = run(db, Operation::Batch { operations }) else {
        panic!("a batch answers with its mutations");
    };
    results
        .iter()
        .map(|r| r.version.unwrap_or_default())
        .collect()
}

fn put(space: &str, id: &str, value: serde_json::Value) -> BatchOperation {
    BatchOperation::JsonPut {
        space: space.into(),
        id: id.into(),
        value,
        if_version: None,
    }
}
