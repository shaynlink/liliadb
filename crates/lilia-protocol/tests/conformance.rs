//! The shared conformance fixtures (`tools/conformance/fixtures.json`) through the executor, as
//! the daemon and the bindings run them; the same fixtures run on Node, the daemon and the CLI.
//!
//! A version is the engine's sequence number of a write, so the fixtures name versions instead
//! of stating them: `"$kv_a1"` binds the version a step returns, later steps compare with it or
//! send it as an `if_version`, and two names never bind the same version.

use std::collections::BTreeMap;

use lilia_core::{BatchOperation, LiliaError};
use lilia_protocol::{execute, Operation, ResponseValue};
use serde_json::{json, Value};

/// The versions the fixtures named so far.
#[derive(Default)]
struct Versions(BTreeMap<String, String>);

impl Versions {
    /// `value` with its named versions replaced by their values (as `if_version` sends them).
    fn substitute(&self, value: &Value) -> Value {
        match value {
            Value::String(name) if name.starts_with('$') => Value::String(
                self.0
                    .get(name)
                    .unwrap_or_else(|| panic!("{name} is not bound yet"))
                    .clone(),
            ),
            Value::Array(items) => {
                Value::Array(items.iter().map(|item| self.substitute(item)).collect())
            }
            Value::Object(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(k, v)| (k.clone(), self.substitute(v)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    /// Whether `actual` matches `expected`, binding the names `expected` uses for the first time.
    fn matches(&mut self, actual: &Value, expected: &Value) -> bool {
        match (actual, expected) {
            (Value::String(version), Value::String(name)) if name.starts_with('$') => {
                if let Some(bound) = self.0.get(name) {
                    return bound == version;
                }
                let taken = self.0.values().any(|bound| bound == version);
                assert!(
                    !taken,
                    "{name} binds {version}, which another name already holds"
                );
                self.0.insert(name.clone(), version.clone());
                true
            }
            (Value::Array(actual), Value::Array(expected)) => {
                actual.len() == expected.len()
                    && actual.iter().zip(expected).all(|(a, e)| self.matches(a, e))
            }
            (Value::Object(actual), Value::Object(expected)) => {
                actual.len() == expected.len()
                    && expected
                        .iter()
                        .all(|(key, e)| actual.get(key).is_some_and(|a| self.matches(a, e)))
            }
            (actual, expected) => actual == expected,
        }
    }
}

#[test]
fn shared_conformance_fixtures() {
    let fixtures: Vec<Value> =
        serde_json::from_str(include_str!("../../../tools/conformance/fixtures.json")).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let database = lilia_protocol::open(directory.path().join("db"), &Default::default()).unwrap();
    let mut versions = Versions::default();
    for step in fixtures {
        let name = step["name"].as_str().unwrap();
        // The request: the step without the result it expects, its named versions replaced.
        let mut request = step.clone();
        request.as_object_mut().unwrap().remove("expected");
        match run(&database, &versions.substitute(&request)) {
            Ok(value) => {
                assert!(step.get("error").is_none(), "{name}: expected an error");
                assert!(
                    versions.matches(&value, &step["expected"]),
                    "{name}: {value} is not {}",
                    step["expected"]
                );
            }
            Err(error) => {
                assert_eq!(
                    json!({"code": error.code, "retryable": error.retryable}),
                    step["error"],
                    "{name}"
                );
                assert!(!error.request_id.is_nil(), "{name}: no request id");
            }
        }
    }
    database.close().unwrap();
}

/// The value the response to `step` carries.
fn run(database: &lilia_db::Database, step: &Value) -> Result<Value, LiliaError> {
    let text = |field: &str| step[field].as_str().unwrap().to_owned();
    let bytes = |field: &str| serde_json::from_value::<Vec<u8>>(step[field].clone()).unwrap();
    let limit = || u32::try_from(step["limit"].as_u64().unwrap()).unwrap();
    let operation = match text("op").as_str() {
        "batch" => Operation::Batch {
            operations: serde_json::from_value::<Vec<BatchOperation>>(step["operations"].clone())
                .map_err(|e| {
                LiliaError::new(lilia_core::ErrorCode::InvalidInput, e.to_string(), false)
            })?,
        },
        "kv_get" => Operation::KvGet {
            namespace: text("namespace"),
            key: bytes("key"),
        },
        "kv_scan" => Operation::KvScan {
            namespace: text("namespace"),
            after: step.get("after").map(|_| bytes("after")),
            limit: limit(),
        },
        "json_get" => Operation::JsonGet {
            space: text("space"),
            id: text("id"),
        },
        "json_scan" => Operation::JsonScan {
            space: text("space"),
            after: step["after"].as_str().map(str::to_owned),
            limit: limit(),
        },
        other => panic!("unknown fixture operation {other}"),
    };
    let response: ResponseValue = execute(database, operation)?;
    let mut tagged = serde_json::to_value(&response).unwrap();
    Ok(tagged["value"].take())
}
