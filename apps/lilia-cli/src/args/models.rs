//! The subcommands of each data model.

use std::path::PathBuf;

use clap::{Subcommand, ValueEnum};

use super::Encoding;

#[derive(Debug, Subcommand)]
pub(crate) enum KvCommand {
    Get {
        namespace: String,
        key: String,
        #[arg(long, default_value = "text")]
        encoding: Encoding,
    },
    Set {
        namespace: String,
        key: String,
        value: String,
        #[arg(long)]
        if_version: Option<u64>,
        #[arg(long)]
        expires_at_ms: Option<i64>,
        /// Encoding of the key and the value, unless `--key-encoding` or `--value-encoding`
        /// says otherwise.
        #[arg(long, default_value = "text")]
        encoding: Encoding,
        #[arg(long)]
        key_encoding: Option<Encoding>,
        #[arg(long)]
        value_encoding: Option<Encoding>,
    },
    Delete {
        namespace: String,
        key: String,
        #[arg(long)]
        if_version: Option<u64>,
        #[arg(long, default_value = "text")]
        encoding: Encoding,
    },
    Scan {
        namespace: String,
        /// Starts after this key.
        #[arg(long)]
        after: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
        #[arg(long, default_value = "text")]
        encoding: Encoding,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum JsonCommand {
    Get {
        space: String,
        id: String,
    },
    Put {
        space: String,
        id: String,
        value: String,
        #[arg(long)]
        if_version: Option<u64>,
    },
    Delete {
        space: String,
        id: String,
        #[arg(long)]
        if_version: Option<u64>,
    },
    Scan {
        space: String,
        #[arg(long)]
        after: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Documents whose fields (paths, or index names) equal the values of a JSON object.
    Find {
        space: String,
        query: String,
        #[arg(long)]
        after: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    /// Index the values of `path` (`$.a.b`, or an expression) under `name`.
    CreateIndex {
        space: String,
        name: String,
        path: String,
        /// Index only the documents this expression holds for.
        #[arg(long)]
        filter: Option<String>,
    },
    DropIndex {
        space: String,
        name: String,
    },
    Indexes {
        space: String,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum LedgerCommand {
    /// The Merkle root of the ledger.
    Root,
    /// The latest commit of the ledger.
    Latest,
    /// A proof that the current version of a JSON document is in the ledger.
    ProveDoc { space: String, id: String },
    /// A proof that the current version of a key is in the ledger.
    ProveKv {
        namespace: String,
        key: String,
        #[arg(long, default_value = "text")]
        encoding: Encoding,
    },
    /// A proof that commit `sequence` (from 1) is in the ledger.
    ProveCommit { sequence: u64 },
    /// Verify a proof file against the current root, or `--expected-root` (hex).
    VerifyProof {
        proof_file: PathBuf,
        #[arg(long)]
        expected_root: Option<String>,
    },
    /// Verify the whole ledger against the live records.
    VerifyDb,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum DirectionArg {
    #[default]
    Outgoing,
    Incoming,
    Both,
}

impl From<DirectionArg> for lilia_core::Direction {
    fn from(arg: DirectionArg) -> Self {
        match arg {
            DirectionArg::Outgoing => Self::Outbound,
            DirectionArg::Incoming => Self::Inbound,
            DirectionArg::Both => Self::Both,
        }
    }
}

#[derive(Debug, Subcommand)]
pub(crate) enum GraphCommand {
    Node {
        graph: String,
        id: String,
        #[arg(long, default_value = "default")]
        kind: String,
        /// JSON metadata.
        #[arg(long)]
        metadata: Option<String>,
    },
    Edge {
        graph: String,
        source: String,
        target: String,
        #[arg(long, default_value = "edge")]
        kind: String,
        #[arg(long, default_value_t = 1.0)]
        weight: f64,
    },
    Neighbors {
        graph: String,
        node: String,
        #[arg(long, default_value = "outgoing")]
        direction: DirectionArg,
        #[arg(long)]
        kind: Option<String>,
    },
    Reach {
        graph: String,
        #[arg(required = true)]
        start_nodes: Vec<String>,
        #[arg(long, default_value = "outgoing")]
        direction: DirectionArg,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum FtsCommand {
    Search {
        collection: String,
        query: String,
        #[arg(long, default_value_t = 10)]
        limit: u32,
    },
    Index {
        collection: String,
        doc_id: String,
        content: String,
    },
    Suggest {
        collection: String,
        prefix: String,
        #[arg(long, default_value_t = 1)]
        max_distance: u8,
        #[arg(long, default_value_t = 10)]
        limit: u32,
    },
    Fuzzy {
        collection: String,
        query: String,
        #[arg(long, default_value_t = 1)]
        max_distance: u8,
        #[arg(long, default_value_t = 10)]
        limit: u32,
    },
    Phonetic {
        collection: String,
        query: String,
        #[arg(long, default_value_t = 10)]
        limit: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum VectorMetricArg {
    Cosine,
    L2,
    Dot,
    Hamming,
}

impl From<VectorMetricArg> for lilia_core::VectorMetric {
    fn from(arg: VectorMetricArg) -> Self {
        match arg {
            VectorMetricArg::Cosine => Self::Cosine,
            VectorMetricArg::L2 => Self::L2,
            VectorMetricArg::Dot => Self::Dot,
            VectorMetricArg::Hamming => Self::Hamming,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum ElementTypeArg {
    #[default]
    Float32,
    Int8,
    Bit,
}

impl From<ElementTypeArg> for lilia_core::VectorElementType {
    fn from(arg: ElementTypeArg) -> Self {
        match arg {
            ElementTypeArg::Float32 => Self::Float32,
            ElementTypeArg::Int8 => Self::Int8,
            ElementTypeArg::Bit => Self::Bit,
        }
    }
}

#[derive(Debug, Subcommand)]
pub(crate) enum VectorCommand {
    CreateIndex {
        collection: String,
        #[arg(long)]
        dim: u32,
        #[arg(long, default_value_t = 16)]
        m: usize,
        #[arg(long, default_value_t = 64)]
        ef_construction: usize,
        #[arg(long, default_value_t = 32)]
        ef_search: usize,
        #[arg(long, value_enum, default_value = "cosine")]
        metric: VectorMetricArg,
        #[arg(long, value_enum, default_value = "float32")]
        element_type: ElementTypeArg,
    },
    IndexInfo {
        collection: String,
    },
    DropIndex {
        collection: String,
    },
    /// Store a vector: a JSON array, comma-separated numbers, or base64 of its bytes.
    Set {
        collection: String,
        id: String,
        vector: String,
        #[arg(long, value_enum, default_value = "float32")]
        element_type: ElementTypeArg,
    },
    Get {
        collection: String,
        id: String,
    },
    Delete {
        collection: String,
        id: String,
    },
    Search {
        collection: String,
        vector: String,
        #[arg(long, default_value_t = 10)]
        limit: u32,
        #[arg(long, value_enum, default_value = "cosine")]
        metric: VectorMetricArg,
        #[arg(long, value_enum, default_value = "float32")]
        element_type: ElementTypeArg,
        /// Search only the nodes reachable from `<graph>:<node>`.
        #[arg(long, conflicts_with = "by_json_index")]
        in_graph_neighbors: Option<String>,
        /// Search only the documents of `<space>:<index>:<value>`.
        #[arg(long)]
        by_json_index: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum HybridCommand {
    Search {
        collection: String,
        #[arg(long)]
        query: Option<String>,
        #[arg(long)]
        vector: Option<String>,
        #[arg(long, default_value_t = 10)]
        limit: u32,
        #[arg(long, default_value_t = 60)]
        rrf_k: u32,
        #[arg(long, default_value_t = 1.0)]
        fts_weight: f64,
        #[arg(long, default_value_t = 1.0)]
        vector_weight: f64,
        #[arg(long, value_enum, default_value = "cosine")]
        metric: VectorMetricArg,
        #[arg(long, value_enum, default_value = "float32")]
        element_type: ElementTypeArg,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum CrdtCommand {
    Counter {
        #[command(subcommand)]
        command: CrdtCounterCommand,
    },
    Set {
        #[command(subcommand)]
        command: CrdtSetCommand,
    },
    Json {
        #[command(subcommand)]
        command: CrdtJsonCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum CrdtCounterCommand {
    /// Read the current value of a PN-Counter.
    Get { space: String, id: String },
    /// Increment a PN-Counter by `amount` on behalf of `actor_id`.
    Inc {
        space: String,
        id: String,
        actor_id: String,
        #[arg(default_value_t = 1)]
        amount: u64,
    },
    /// Decrement a PN-Counter by `amount` on behalf of `actor_id`.
    Dec {
        space: String,
        id: String,
        actor_id: String,
        #[arg(default_value_t = 1)]
        amount: u64,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum CrdtSetCommand {
    /// Read all live elements of an OR-Set.
    Get { space: String, id: String },
    /// Test whether an element is in an OR-Set.
    Contains {
        space: String,
        id: String,
        element: String,
    },
    /// Add an element to an OR-Set on behalf of `actor_id`.
    Add {
        space: String,
        id: String,
        actor_id: String,
        element: String,
    },
    /// Remove an element from an OR-Set (tombstones all its tags).
    Remove {
        space: String,
        id: String,
        element: String,
    },
    /// Prune tombstones older than `cutoff_ms` from an OR-Set.
    Prune {
        space: String,
        id: String,
        cutoff_ms: i64,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum CrdtJsonCommand {
    /// Read the materialised JSON document.
    Get { space: String, id: String },
    /// Set a field (dot-separated path) in an LWW-JSON document.
    SetField {
        space: String,
        id: String,
        actor_id: String,
        path: String,
        /// JSON-encoded value (e.g. `'"hello"'` or `'42'`).
        value: String,
    },
    /// Delete a field from an LWW-JSON document.
    DeleteField {
        space: String,
        id: String,
        actor_id: String,
        path: String,
    },
}

/// Times are ISO 8601 dates or date-times, or `now`, `infinity`, `-infinity`.
#[derive(Debug, Subcommand)]
pub(crate) enum BitempCommand {
    /// Record a JSON value valid from `--from` to `--to` (inclusive days).
    Put {
        space: String,
        id: String,
        value: String,
        #[arg(long = "from")]
        vt_from: String,
        #[arg(long = "to")]
        vt_to: String,
        /// Record it at this transaction time instead of now.
        #[arg(long = "system-time")]
        system_time: Option<String>,
    },
    /// The value valid at `--for-vt`, as known at `--as-of-tt`.
    Get {
        space: String,
        id: String,
        #[arg(long = "as-of-tt", default_value = "now")]
        as_of_tt: String,
        #[arg(long = "for-vt", default_value = "now")]
        for_vt: String,
    },
    /// Every interval of the entity, as known at `--as-of-tt`.
    Timeline {
        space: String,
        id: String,
        #[arg(long = "as-of-tt", default_value = "now")]
        as_of_tt: String,
    },
    /// Every version ever recorded of the entity.
    History { space: String, id: String },
    /// End the entity's validity, over `--from`..`--to` or entirely.
    Delete {
        space: String,
        id: String,
        #[arg(long = "from", requires = "vt_to")]
        vt_from: Option<String>,
        #[arg(long = "to", requires = "vt_from")]
        vt_to: Option<String>,
    },
}
