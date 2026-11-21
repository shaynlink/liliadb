//! SQL over the data models (feature `sql`): read-only `DataFusion` queries
//! ([`crate::Session::sql`]).
//!
//! A query reads the tables it names, built for it from one fixed state of the database through
//! the caller's session: read policies and masking apply to every row, and a session in the past
//! ([`crate::Session::at_version`]) queries the past. The tables:
//!
//! | table | columns |
//! |---|---|
//! | `lilia_kv` | `namespace`, `key` (bytes), `value` (bytes), `version`, `expires_at_ms` |
//! | `lilia_json` | `space`, `id`, `value` (JSON text), `version` |
//! | any other name | the documents of the space of that name: `id`, `value`, `version` |
//! | `lilia_graph_nodes` | `graph`, `id`, `kind`, `metadata` (JSON text) |
//! | `lilia_graph_edges` | `graph`, `source`, `target`, `kind`, `weight` |
//! | `lilia_timeseries` | `series`, `timestamp`, `value` (raw points) |
//!
//! JSON text is read with the JSON functions (`json_get_str(value, 'name')`, `value->>'name'`).
//! Parameters bind to `$1`, `$2`, .... Writes go through the model APIs: SQL statements that
//! write are refused. A query reads every row of the tables it names.

use std::collections::BTreeSet;
use std::sync::Arc;

use lilia_analytics::arrow::array::{
    ArrayRef, BinaryBuilder, Float64Builder, Int64Builder, RecordBatch, StringBuilder,
};
use lilia_analytics::arrow::datatypes::{DataType, Field, Schema};
use lilia_analytics::QueryTable;
use lilia_core::SqlQueryResult;
use serde_json::Value;

use crate::context::sealed::Inner;
use crate::context::Reader;
use crate::error::{Error, Result};
use crate::keys::{self, validate_name};
use crate::read::{self, Scan};
use crate::session::Session;

impl Session<'_> {
    /// Runs the read-only SQL query `query`, with `params` bound to its placeholders; see the
    /// [module documentation](crate::sql) for the tables.
    ///
    /// # Errors
    ///
    /// [`Error::Sql`] for SQL that does not parse, names unknown columns, writes or fails to
    /// run; invalid names; records that do not decode; engine errors.
    pub fn sql(&self, query: &str, params: &[Value]) -> Result<SqlQueryResult> {
        let engine = self.database().analytics()?;
        let pinned = self.pinned();
        let reader = pinned.reader(true);
        let mut named = BTreeSet::new();
        let mut tables = Vec::new();
        for name in engine.table_references(query)? {
            if named.insert(name.clone()) {
                tables.push(table(&pinned, &reader, &name)?);
            }
        }
        let output = engine.query_tables(tables, query, params)?;
        Ok(SqlQueryResult {
            columns: output.column_names(),
            rows: output.to_json_rows()?,
            rows_affected: 0,
        })
    }
}

/// A version as a SQL integer.
fn version(version: u64) -> i64 {
    i64::try_from(version).unwrap_or(i64::MAX)
}

/// Table `name`, read through `session`.
fn table(session: &Session<'_>, reader: &Reader<'_>, name: &str) -> Result<QueryTable> {
    let (fields, arrays) = match name {
        "lilia_kv" => kv(session, reader)?,
        "lilia_json" => json(session, reader)?,
        "lilia_graph_nodes" => nodes(session, reader)?,
        "lilia_graph_edges" => edges(session, reader)?,
        "lilia_timeseries" => series(session, reader)?,
        space => documents(session, space)?,
    };
    let schema = Arc::new(Schema::new(
        fields
            .into_iter()
            .map(|(name, ty, nullable)| Field::new(name, ty, nullable))
            .collect::<Vec<_>>(),
    ));
    let batch =
        RecordBatch::try_new(Arc::clone(&schema), arrays).map_err(|e| Error::Sql(e.to_string()))?;
    Ok(QueryTable {
        name: name.to_owned(),
        schema,
        batches: vec![batch],
    })
}

/// The columns of a table, as `(name, type, nullable)`, and their arrays.
type Columns = (Vec<(&'static str, DataType, bool)>, Vec<ArrayRef>);

fn kv(session: &Session<'_>, reader: &Reader<'_>) -> Result<Columns> {
    let (mut ns, mut key, mut value, mut ver, mut expiry) = (
        StringBuilder::new(),
        BinaryBuilder::new(),
        BinaryBuilder::new(),
        Int64Builder::new(),
        Int64Builder::new(),
    );
    for namespace in read::collections(reader, keys::KV)? {
        for entry in session.kv_scan(&namespace, &Scan::all())? {
            ns.append_value(&namespace);
            key.append_value(&entry.key);
            value.append_value(&entry.value);
            ver.append_value(version(entry.version));
            expiry.append_option(entry.expires_at_ms);
        }
    }
    Ok((
        vec![
            ("namespace", DataType::Utf8, false),
            ("key", DataType::Binary, false),
            ("value", DataType::Binary, false),
            ("version", DataType::Int64, false),
            ("expires_at_ms", DataType::Int64, true),
        ],
        vec![
            Arc::new(ns.finish()),
            Arc::new(key.finish()),
            Arc::new(value.finish()),
            Arc::new(ver.finish()),
            Arc::new(expiry.finish()),
        ],
    ))
}

fn json(session: &Session<'_>, reader: &Reader<'_>) -> Result<Columns> {
    let (mut space, mut id, mut value, mut ver) = (
        StringBuilder::new(),
        StringBuilder::new(),
        StringBuilder::new(),
        Int64Builder::new(),
    );
    for name in read::collections(reader, keys::DOC)? {
        for entry in session.json_scan(&name, &Scan::all())? {
            space.append_value(&name);
            id.append_value(&entry.id);
            value.append_value(entry.value().to_string());
            ver.append_value(version(entry.version));
        }
    }
    Ok((
        vec![
            ("space", DataType::Utf8, false),
            ("id", DataType::Utf8, false),
            ("value", DataType::Utf8, false),
            ("version", DataType::Int64, false),
        ],
        vec![
            Arc::new(space.finish()),
            Arc::new(id.finish()),
            Arc::new(value.finish()),
            Arc::new(ver.finish()),
        ],
    ))
}

/// The documents of `space`.
fn documents(session: &Session<'_>, space: &str) -> Result<Columns> {
    validate_name("table", space)?;
    let (mut id, mut value, mut ver) = (
        StringBuilder::new(),
        StringBuilder::new(),
        Int64Builder::new(),
    );
    for entry in session.json_scan(space, &Scan::all())? {
        id.append_value(&entry.id);
        value.append_value(entry.value().to_string());
        ver.append_value(version(entry.version));
    }
    Ok((
        vec![
            ("id", DataType::Utf8, false),
            ("value", DataType::Utf8, false),
            ("version", DataType::Int64, false),
        ],
        vec![
            Arc::new(id.finish()),
            Arc::new(value.finish()),
            Arc::new(ver.finish()),
        ],
    ))
}

fn nodes(session: &Session<'_>, reader: &Reader<'_>) -> Result<Columns> {
    let (mut graph, mut id, mut kind, mut metadata) = (
        StringBuilder::new(),
        StringBuilder::new(),
        StringBuilder::new(),
        StringBuilder::new(),
    );
    for name in read::collections(reader, keys::GRAPH_NODE)? {
        for node in session.graph(&name).nodes(None, &Scan::all())? {
            graph.append_value(&name);
            id.append_value(&node.id);
            kind.append_value(&node.kind);
            metadata.append_value(node.metadata.to_string());
        }
    }
    Ok((
        vec![
            ("graph", DataType::Utf8, false),
            ("id", DataType::Utf8, false),
            ("kind", DataType::Utf8, false),
            ("metadata", DataType::Utf8, false),
        ],
        vec![
            Arc::new(graph.finish()),
            Arc::new(id.finish()),
            Arc::new(kind.finish()),
            Arc::new(metadata.finish()),
        ],
    ))
}

fn edges(session: &Session<'_>, reader: &Reader<'_>) -> Result<Columns> {
    let (mut graph, mut source, mut target, mut kind, mut weight) = (
        StringBuilder::new(),
        StringBuilder::new(),
        StringBuilder::new(),
        StringBuilder::new(),
        Float64Builder::new(),
    );
    for name in read::collections(reader, keys::EDGE_OUT)? {
        for edge in session.graph(&name).edges(None, None, None, None)? {
            graph.append_value(&name);
            source.append_value(&edge.source);
            target.append_value(&edge.target);
            kind.append_value(&edge.kind);
            weight.append_value(edge.weight);
        }
    }
    Ok((
        vec![
            ("graph", DataType::Utf8, false),
            ("source", DataType::Utf8, false),
            ("target", DataType::Utf8, false),
            ("kind", DataType::Utf8, false),
            ("weight", DataType::Float64, false),
        ],
        vec![
            Arc::new(graph.finish()),
            Arc::new(source.finish()),
            Arc::new(target.finish()),
            Arc::new(kind.finish()),
            Arc::new(weight.finish()),
        ],
    ))
}

fn series(session: &Session<'_>, reader: &Reader<'_>) -> Result<Columns> {
    let (mut series, mut timestamp, mut value) = (
        StringBuilder::new(),
        Int64Builder::new(),
        Float64Builder::new(),
    );
    for name in read::collections(reader, keys::TS_CHUNK)? {
        for point in session.series(&name).range(i64::MIN, i64::MAX)? {
            series.append_value(&name);
            timestamp.append_value(point.timestamp);
            value.append_value(point.value);
        }
    }
    Ok((
        vec![
            ("series", DataType::Utf8, false),
            ("timestamp", DataType::Int64, false),
            ("value", DataType::Float64, false),
        ],
        vec![
            Arc::new(series.finish()),
            Arc::new(timestamp.finish()),
            Arc::new(value.finish()),
        ],
    ))
}
