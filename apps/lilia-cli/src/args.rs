use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use lilia_protocol::OpenOptions;

#[derive(Debug, Parser)]
#[command(name = "lilia", version, about = "Local-first multimodel database")]
pub(crate) struct Arguments {
    #[arg(long, global = true, default_value = "human")]
    pub(crate) output: Output,
    /// Directory of the database; created if missing.
    #[arg(long, global = true, default_value = "lilia.lilia")]
    pub(crate) database: PathBuf,
    #[command(flatten)]
    pub(crate) open: OpenArgs,
    #[command(subcommand)]
    pub(crate) command: Command,
}

/// How the database opens: the options of the daemon and the bindings.
#[derive(Debug, Clone, Args)]
pub(crate) struct OpenArgs {
    /// `durable` (default), `balanced` or `performance`.
    #[arg(long, global = true)]
    pub(crate) durability: Option<String>,
    /// `server` (default), `mobile` or `constrained`.
    #[arg(long, global = true)]
    pub(crate) profile: Option<String>,
    /// Keep a tamper-evident ledger of every record write; a database that keeps one must
    /// always be opened with it.
    #[arg(long, global = true)]
    pub(crate) ledger: bool,
    /// How long old versions stay readable, in milliseconds (a week by default).
    #[arg(long, global = true)]
    pub(crate) history_retention_ms: Option<u64>,
    /// Commits that can be undone (none by default).
    #[arg(long, global = true, default_value_t = 0)]
    pub(crate) undo_depth: u32,
}

impl OpenArgs {
    pub(crate) fn options(&self) -> OpenOptions {
        OpenOptions {
            durability: self.durability.clone(),
            profile: self.profile.clone(),
            ledger: self.ledger,
            history_retention_ms: self.history_retention_ms,
            undo_depth: self.undo_depth,
        }
    }

    /// The same options as `lilia-daemon` arguments.
    pub(crate) fn daemon_arguments(&self) -> Vec<String> {
        let mut arguments = Vec::new();
        if let Some(durability) = &self.durability {
            arguments.extend(["--durability".to_owned(), durability.clone()]);
        }
        if let Some(profile) = &self.profile {
            arguments.extend(["--profile".to_owned(), profile.clone()]);
        }
        if self.ledger {
            arguments.push("--ledger".to_owned());
        }
        if let Some(retention) = self.history_retention_ms {
            arguments.extend(["--history-retention-ms".to_owned(), retention.to_string()]);
        }
        arguments.extend(["--undo-depth".to_owned(), self.undo_depth.to_string()]);
        arguments
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Output {
    Human,
    Json,
    Jsonl,
}

/// How a key, value or cursor given on the command line encodes its bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum Encoding {
    #[default]
    Text,
    Hex,
    Base64,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// The database's daemon, which serves it to other processes.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Zero-knowledge proofs on documents.
    Zk {
        #[command(subcommand)]
        command: ZkCommand,
    },
    #[command(flatten)]
    Database(DatabaseCommand),
}

/// The commands that open the database.
#[derive(Debug, Subcommand)]
pub(crate) enum DatabaseCommand {
    /// Create the database if missing.
    Init,
    /// Check the database's integrity and show its storage.
    Doctor {
        /// Read every stored entry back (and verify the ledger strictly) rather than checking
        /// the ledger alone.
        #[arg(long)]
        deep: bool,
    },
    Kv {
        #[command(subcommand)]
        command: KvCommand,
    },
    Json {
        #[command(subcommand)]
        command: JsonCommand,
    },
    /// Commit the operations of a JSON file atomically.
    Batch { file: PathBuf },
    /// Run a read-only SQL query; `--param` binds `$1`, `$2`... (JSON, or text).
    Sql {
        query: String,
        #[arg(long = "param")]
        params: Vec<String>,
    },
    Graph {
        #[command(subcommand)]
        command: GraphCommand,
    },
    Fts {
        #[command(subcommand)]
        command: FtsCommand,
    },
    Vector {
        #[command(subcommand)]
        command: VectorCommand,
    },
    Hybrid {
        #[command(subcommand)]
        command: HybridCommand,
    },
    Bitemp {
        #[command(subcommand)]
        command: BitempCommand,
    },
    Crdt {
        #[command(subcommand)]
        command: CrdtCommand,
    },
    Ledger {
        #[command(subcommand)]
        command: LedgerCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum DaemonCommand {
    Start {
        #[arg(long)]
        endpoint: Option<PathBuf>,
        #[arg(long)]
        token_file: Option<PathBuf>,
        #[arg(long)]
        foreground: bool,
    },
    Status {
        #[arg(long)]
        endpoint: Option<PathBuf>,
        #[arg(long)]
        token_file: Option<PathBuf>,
    },
    Stop {
        #[arg(long)]
        endpoint: Option<PathBuf>,
        #[arg(long)]
        token_file: Option<PathBuf>,
    },
}

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

#[derive(Debug, Subcommand)]
pub(crate) enum ZkCommand {
    /// Generate the proving and verifying keys of the `solde_age` circuit.
    Setup {
        #[arg(long)]
        out_pk: PathBuf,
        #[arg(long)]
        out_vk: PathBuf,
    },
    /// The Poseidon commitment to a document's `balance`, `age` and salt.
    Commit {
        space: String,
        id: String,
        #[arg(long)]
        salt: Option<String>,
    },
    /// Prove a condition on a document without revealing it, such as
    /// "balance >= 1000 AND age >= 18".
    Prove {
        space: String,
        id: String,
        #[arg(long)]
        condition: String,
        #[arg(long)]
        salt: Option<String>,
        #[arg(long)]
        pk: Option<PathBuf>,
    },
    /// Verify a proof, with the circuit's default keys or `--vk`.
    Verify {
        #[arg(long)]
        proof: PathBuf,
        #[arg(long)]
        vk: Option<PathBuf>,
    },
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
