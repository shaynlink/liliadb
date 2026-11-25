//! The commands that are operations of the protocol: each becomes an [`Operation`] (a write, a
//! batch of one) that [`lilia_protocol::execute`] runs, as the daemon and the bindings do.

use std::fs;
use std::path::Path;

use lilia_core::{BatchOperation, HnswConfig, LiliaError, VectorElementType};
use lilia_db::Database;
use lilia_protocol::{Operation, ResponseValue};
use serde_json::{json, Value};

use super::input::{self, invalid};
use crate::args::models::{
    CrdtCommand, CrdtCounterCommand, CrdtJsonCommand, CrdtSetCommand, FtsCommand, GraphCommand,
    HybridCommand, JsonCommand, KvCommand, VectorCommand,
};

/// A command as the protocol runs it.
pub(crate) enum Call {
    Read(Operation),
    /// One write; its outcome is the command's result.
    Write(BatchOperation),
    /// Several writes, committed atomically; their outcomes are the result.
    Batch(Vec<BatchOperation>),
}

/// Runs `call` and returns what its response carries.
pub(crate) fn run(database: &Database, call: Call) -> Result<Value, LiliaError> {
    let (operation, single) = match call {
        Call::Read(operation) => (operation, false),
        Call::Write(write) => (
            Operation::Batch {
                operations: vec![write],
            },
            true,
        ),
        Call::Batch(operations) => (Operation::Batch { operations }, false),
    };
    let response = lilia_protocol::execute(database, operation)?;
    let value = value(&response)?;
    Ok(match (single, value) {
        (true, Value::Array(mut outcomes)) if outcomes.len() == 1 => outcomes.remove(0),
        (_, value) => value,
    })
}

/// The value a response carries: `{"ok": true}` for an acknowledgement.
fn value(response: &ResponseValue) -> Result<Value, LiliaError> {
    let mut tagged = serde_json::to_value(response).map_err(|e| {
        LiliaError::new(
            lilia_core::ErrorCode::Storage,
            format!("cannot render the response: {e}"),
            false,
        )
    })?;
    Ok(tagged
        .get_mut("value")
        .map_or_else(|| json!({"ok": true}), Value::take))
}

pub(crate) fn kv(command: KvCommand) -> Result<Call, LiliaError> {
    Ok(match command {
        KvCommand::Get {
            namespace,
            key,
            encoding,
        } => Call::Read(Operation::KvGet {
            namespace,
            key: input::bytes(&key, encoding, "key")?,
        }),
        KvCommand::Set {
            namespace,
            key,
            value,
            if_version,
            expires_at_ms,
            encoding,
            key_encoding,
            value_encoding,
        } => Call::Write(BatchOperation::KvSet {
            namespace,
            key: input::bytes(&key, key_encoding.unwrap_or(encoding), "key")?,
            value: input::bytes(&value, value_encoding.unwrap_or(encoding), "value")?,
            if_version,
            expires_at_ms,
        }),
        KvCommand::Delete {
            namespace,
            key,
            if_version,
            encoding,
        } => Call::Write(BatchOperation::KvDelete {
            namespace,
            key: input::bytes(&key, encoding, "key")?,
            if_version,
        }),
        KvCommand::Scan {
            namespace,
            after,
            limit,
            encoding,
        } => Call::Read(Operation::KvScan {
            namespace,
            after: after
                .map(|after| input::bytes(&after, encoding, "after"))
                .transpose()?,
            limit,
        }),
    })
}

pub(crate) fn json(command: JsonCommand) -> Result<Call, LiliaError> {
    Ok(match command {
        JsonCommand::Get { space, id } => Call::Read(Operation::JsonGet { space, id }),
        JsonCommand::Put {
            space,
            id,
            value,
            if_version,
        } => Call::Write(BatchOperation::JsonPut {
            space,
            id,
            value: input::json(&value, "document")?,
            if_version,
        }),
        JsonCommand::Delete {
            space,
            id,
            if_version,
        } => Call::Write(BatchOperation::JsonDelete {
            space,
            id,
            if_version,
        }),
        JsonCommand::Scan {
            space,
            after,
            limit,
        } => Call::Read(Operation::JsonScan {
            space,
            after,
            limit,
        }),
        JsonCommand::Find {
            space,
            query,
            after,
            limit,
        } => {
            let query = input::json(&query, "query")?;
            if !query.is_object() {
                return Err(invalid("a query is a JSON object of fields and values"));
            }
            Call::Read(Operation::JsonFind {
                space,
                query,
                after,
                limit,
            })
        }
        JsonCommand::CreateIndex {
            space,
            name,
            path,
            filter,
        } => Call::Read(Operation::JsonCreateIndex {
            space,
            name,
            path,
            filter,
        }),
        JsonCommand::DropIndex { space, name } => {
            Call::Read(Operation::JsonDropIndex { space, name })
        }
        JsonCommand::Indexes { space } => Call::Read(Operation::JsonListIndexes { space }),
    })
}

/// The operations of the JSON file `file`, as `lilia batch` and the bindings' `batch` take them.
pub(crate) fn batch(file: &Path) -> anyhow::Result<Call> {
    let operations: Vec<BatchOperation> = serde_json::from_slice(&fs::read(file)?)?;
    Ok(Call::Batch(operations))
}

/// `params` as JSON values; one that is not JSON is the text it holds.
pub(crate) fn sql(query: String, params: &[String]) -> Call {
    let params = params
        .iter()
        .map(|param| serde_json::from_str(param).unwrap_or_else(|_| Value::String(param.clone())))
        .collect();
    Call::Read(Operation::SqlQuery { sql: query, params })
}

pub(crate) fn graph(command: GraphCommand) -> Result<Call, LiliaError> {
    Ok(match command {
        GraphCommand::Node {
            graph,
            id,
            kind,
            metadata,
        } => Call::Write(BatchOperation::GraphSetNode {
            graph,
            id,
            kind,
            metadata: metadata
                .map(|metadata| input::json(&metadata, "metadata"))
                .transpose()?
                .unwrap_or_else(|| json!({})),
            doc_space: None,
            doc_id: None,
        }),
        GraphCommand::Edge {
            graph,
            source,
            target,
            kind,
            weight,
        } => Call::Write(BatchOperation::GraphAddEdge {
            graph,
            source,
            target,
            kind,
            weight,
        }),
        GraphCommand::Neighbors {
            graph,
            node,
            direction,
            kind,
        } => Call::Read(Operation::GraphNeighbors {
            graph,
            node,
            direction: direction.into(),
            kind,
        }),
        GraphCommand::Reach {
            graph,
            start_nodes,
            direction,
        } => Call::Read(Operation::GraphReachability {
            graph,
            start_nodes,
            direction: direction.into(),
        }),
    })
}

pub(crate) fn fts(command: FtsCommand) -> Call {
    match command {
        FtsCommand::Index {
            collection,
            doc_id,
            content,
        } => Call::Write(BatchOperation::FtsIndex {
            collection,
            doc_id,
            content,
        }),
        FtsCommand::Search {
            collection,
            query,
            limit,
        } => Call::Read(Operation::FtsSearch {
            collection,
            query,
            limit,
        }),
        FtsCommand::Suggest {
            collection,
            prefix,
            max_distance,
            limit,
        } => Call::Read(Operation::FtsSuggest {
            collection,
            prefix,
            max_distance,
            limit,
        }),
        FtsCommand::Fuzzy {
            collection,
            query,
            max_distance,
            limit,
        } => Call::Read(Operation::FtsFuzzySearch {
            collection,
            query,
            max_distance,
            limit,
        }),
        FtsCommand::Phonetic {
            collection,
            query,
            limit,
        } => Call::Read(Operation::FtsPhoneticSearch {
            collection,
            query,
            limit,
        }),
    }
}

/// `scope` split in `parts` parts at `:`; `what` describes it in errors.
fn split<'a, const N: usize>(scope: &'a str, what: &str) -> Result<[&'a str; N], LiliaError> {
    let parts: Vec<&str> = scope.splitn(N, ':').collect();
    parts
        .try_into()
        .map_err(|_| invalid(format!("expected {what}, got `{scope}`")))
}

pub(crate) fn vector(command: VectorCommand) -> Result<Call, LiliaError> {
    Ok(match command {
        VectorCommand::CreateIndex {
            collection,
            dim,
            m,
            ef_construction,
            ef_search,
            metric,
            element_type,
        } => Call::Read(Operation::VectorCreateIndex {
            collection,
            config: HnswConfig {
                m,
                ef_construction,
                ef_search,
                metric: metric.into(),
                element_type: element_type.into(),
                dim,
            },
        }),
        VectorCommand::IndexInfo { collection } => {
            Call::Read(Operation::VectorGetIndexInfo { collection })
        }
        VectorCommand::DropIndex { collection } => {
            Call::Read(Operation::VectorDropIndex { collection })
        }
        VectorCommand::Set {
            collection,
            id,
            vector,
            element_type,
        } => {
            let element_type: VectorElementType = element_type.into();
            let (vector, dim) = input::vector(&vector, element_type)?;
            Call::Write(BatchOperation::VectorSet {
                collection,
                id,
                dim,
                element_type,
                vector,
            })
        }
        VectorCommand::Get { collection, id } => {
            Call::Read(Operation::VectorGet { collection, id })
        }
        VectorCommand::Delete { collection, id } => {
            Call::Write(BatchOperation::VectorDelete { collection, id })
        }
        VectorCommand::Search {
            collection,
            vector,
            limit,
            metric,
            element_type,
            in_graph_neighbors,
            by_json_index,
        } => {
            let element_type: VectorElementType = element_type.into();
            let metric = metric.into();
            let (query_vector, dim) = input::vector(&vector, element_type)?;
            if let Some(scope) = in_graph_neighbors {
                let [graph, start_node] = split::<2>(&scope, "<graph>:<node>")?;
                Call::Read(Operation::VectorSearchInGraphNeighbors {
                    collection,
                    graph: graph.to_owned(),
                    start_node: start_node.to_owned(),
                    direction: lilia_core::Direction::Outbound,
                    query_vector,
                    dim,
                    element_type,
                    metric,
                    limit,
                })
            } else if let Some(scope) = by_json_index {
                let [space, index_name, index_value] =
                    split::<3>(&scope, "<space>:<index>:<value>")?;
                Call::Read(Operation::VectorSearchByJsonIndex {
                    collection,
                    space: space.to_owned(),
                    index_name: index_name.to_owned(),
                    index_value: index_value.to_owned(),
                    query_vector,
                    dim,
                    element_type,
                    metric,
                    limit,
                })
            } else {
                Call::Read(Operation::VectorSearch {
                    collection,
                    vector: query_vector,
                    dim,
                    element_type,
                    metric,
                    limit,
                })
            }
        }
    })
}

pub(crate) fn hybrid(command: HybridCommand) -> Result<Call, LiliaError> {
    let HybridCommand::Search {
        collection,
        query,
        vector,
        limit,
        rrf_k,
        fts_weight,
        vector_weight,
        metric,
        element_type,
    } = command;
    if query.is_none() && vector.is_none() {
        return Err(invalid("a hybrid search needs --query, --vector, or both"));
    }
    let element_type: VectorElementType = element_type.into();
    let vector = vector
        .map(|raw| input::vector(&raw, element_type))
        .transpose()?;
    let has_vector = vector.is_some();
    let (vector, dim) = vector.map_or((None, None), |(bytes, dim)| (Some(bytes), Some(dim)));
    Ok(Call::Read(Operation::HybridSearch {
        collection,
        fts_query: query,
        vector,
        dim,
        element_type: has_vector.then_some(element_type),
        metric: has_vector.then(|| metric.into()),
        rrf_k,
        fts_weight,
        vector_weight,
        limit,
    }))
}

pub(crate) fn crdt(command: CrdtCommand) -> Result<Call, LiliaError> {
    Ok(match command {
        CrdtCommand::Counter { command } => match command {
            CrdtCounterCommand::Get { space, id } => {
                Call::Read(Operation::CrdtCounterGet { space, id })
            }
            CrdtCounterCommand::Inc {
                space,
                id,
                actor_id,
                amount,
            } => Call::Write(BatchOperation::CrdtPnCounterIncrement {
                space,
                id,
                actor_id,
                amount,
            }),
            CrdtCounterCommand::Dec {
                space,
                id,
                actor_id,
                amount,
            } => Call::Write(BatchOperation::CrdtPnCounterDecrement {
                space,
                id,
                actor_id,
                amount,
            }),
        },
        CrdtCommand::Set { command } => match command {
            CrdtSetCommand::Get { space, id } => Call::Read(Operation::CrdtSetGet { space, id }),
            CrdtSetCommand::Contains { space, id, element } => {
                Call::Read(Operation::CrdtSetContains { space, id, element })
            }
            CrdtSetCommand::Add {
                space,
                id,
                actor_id,
                element,
            } => Call::Write(BatchOperation::CrdtOrSetAdd {
                space,
                id,
                actor_id,
                element,
            }),
            CrdtSetCommand::Remove { space, id, element } => {
                Call::Write(BatchOperation::CrdtOrSetRemove {
                    space,
                    id,
                    element,
                    timestamp_ms: None,
                })
            }
            CrdtSetCommand::Prune {
                space,
                id,
                cutoff_ms,
            } => Call::Read(Operation::CrdtSetPrune {
                space,
                id,
                cutoff_ms,
            }),
        },
        CrdtCommand::Json { command } => match command {
            CrdtJsonCommand::Get { space, id } => Call::Read(Operation::CrdtJsonGet { space, id }),
            CrdtJsonCommand::SetField {
                space,
                id,
                actor_id,
                path,
                value,
            } => Call::Write(BatchOperation::CrdtJsonSetField {
                space,
                id,
                actor_id,
                path,
                value: input::json(&value, "value")?,
                timestamp_ms: None,
            }),
            CrdtJsonCommand::DeleteField {
                space,
                id,
                actor_id,
                path,
            } => Call::Write(BatchOperation::CrdtJsonDeleteField {
                space,
                id,
                actor_id,
                path,
                timestamp_ms: None,
            }),
        },
    })
}
