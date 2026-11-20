use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "lilia", version, about = "Local-first multimodel database")]
pub(crate) struct Arguments {
    #[arg(long, global = true, default_value = "human")]
    pub(crate) output: Output,
    #[arg(long, global = true, default_value = "lilia.lilia")]
    pub(crate) database: PathBuf,
    #[arg(long = "plugin", global = true)]
    pub(crate) plugin: Option<PathBuf>,
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Output {
    Human,
    Json,
    Jsonl,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum Encoding {
    #[default]
    Text,
    Hex,
    Base64,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    Init,
    Doctor {
        #[arg(long)]
        deep: bool,
    },
    Backup {
        destination: PathBuf,
    },
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    Kv {
        #[command(subcommand)]
        command: KvCommand,
    },
    Json {
        #[command(subcommand)]
        command: JsonCommand,
    },
    Batch {
        file: PathBuf,
    },
    Sql {
        query: String,
        #[arg(long = "param")]
        params: Vec<String>,
    },
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
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
    Blob {
        #[command(subcommand)]
        command: BlobCommand,
    },
    Integrity {
        #[command(subcommand)]
        command: IntegrityCommand,
    },
    Ledger {
        #[command(subcommand)]
        command: LedgerCommand,
    },
    Zk {
        #[command(subcommand)]
        command: ZkCommand,
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
        #[arg(long)]
        key_encoding: Option<Encoding>,
    },
    Set {
        namespace: String,
        key: String,
        value: String,
        #[arg(long)]
        if_version: Option<u64>,
        #[arg(long)]
        expires_at_ms: Option<i64>,
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
        #[arg(long)]
        key_encoding: Option<Encoding>,
    },
    Scan {
        namespace: String,
        #[arg(long)]
        after: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
        #[arg(long, default_value = "text")]
        encoding: Encoding,
        #[arg(long)]
        key_encoding: Option<Encoding>,
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
    CreateCollection {
        space: String,
        #[arg(long)]
        check: Option<String>,
    },
    SetCheck {
        space: String,
        #[arg(long)]
        check: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum IntegrityCommand {
    AddGraphRef {
        graph: String,
        target: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value = "cascade")]
        on_delete: String,
        #[arg(long)]
        required: bool,
    },
    AddKvRef {
        namespace: String,
        target: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value = "restrict")]
        on_delete: String,
        #[arg(long)]
        prefix_separator: Option<char>,
    },
    DropRef {
        name: String,
    },
    ListRefs,
    ListChecks,
}

#[derive(Debug, Subcommand)]
pub(crate) enum LedgerCommand {
    /// Show the current 32-byte cryptographic Merkle Root (BLAKE3) of the database
    Root,
    /// Show the latest commit block recorded in the Merkle Ledger
    Latest,
    /// Export a cryptographic Inclusion Proof for a JSON document
    ProveDoc {
        space: String,
        id: String,
        #[arg(long)]
        version: Option<u64>,
    },
    /// Export a cryptographic Inclusion Proof for a KV key
    ProveKv {
        namespace: String,
        key: String,
        #[arg(long, default_value = "text")]
        encoding: Encoding,
        #[arg(long)]
        version: Option<u64>,
    },
    /// Export a cryptographic Inclusion Proof for a commit sequence number
    ProveCommit { commit_seq: u64 },
    /// Mathematically verify a JSON inclusion proof against the current root or an expected root
    VerifyProof {
        proof_file: PathBuf,
        #[arg(long)]
        expected_root: Option<String>,
    },
    /// Verify the entire Merkle DAG and live records, checking for tampering or corruption
    VerifyDb,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ZkCommand {
    /// Generate a cryptographic proving key (PK) and verifying key (VK) for circuit templates
    Setup {
        #[arg(long, default_value = "solde_age")]
        template: String,
        #[arg(long)]
        out_pk: PathBuf,
        #[arg(long)]
        out_vk: PathBuf,
    },
    /// Export the Poseidon cryptographic commitment anchor for a document
    Commit {
        space: String,
        id: String,
        #[arg(long)]
        salt: Option<String>,
    },
    /// Generate a Zero-Knowledge Proof certifying a condition (e.g. "balance >= 1000 AND age >= 18")
    /// on a private document without revealing the private values
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
    /// Verify a Zero-Knowledge Proof mathematically in < 1 ms without seeing private data
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
    Outbound,
    Inbound,
    Both,
}

impl From<DirectionArg> for lilia_core::Direction {
    fn from(arg: DirectionArg) -> Self {
        match arg {
            DirectionArg::Outbound => Self::Outbound,
            DirectionArg::Inbound => Self::Inbound,
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
        #[arg(long)]
        metadata: Option<String>,
    },
    Edge {
        graph: String,
        source: String,
        target: String,
        #[arg(long, default_value = "edge")]
        kind: String,
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

#[derive(Debug, Subcommand)]
pub(crate) enum VectorCommand {
    CreateIndex {
        collection: String,
        #[arg(long, default_value_t = 16)]
        m: usize,
        #[arg(long, default_value_t = 64)]
        ef_construction: usize,
        #[arg(long, default_value_t = 32)]
        ef_search: usize,
        #[arg(long, value_enum, default_value = "cosine")]
        metric: VectorMetricArg,
        #[arg(long, default_value = "f32")]
        element_type: String,
        #[arg(long, default_value_t = 0)]
        dim: u32,
    },
    IndexInfo {
        collection: String,
    },
    DropIndex {
        collection: String,
    },
    Set {
        collection: String,
        id: String,
        vector: String,
        #[arg(long, default_value = "f32")]
        element_type: String,
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
        #[arg(long, default_value = "f32")]
        element_type: String,
        #[arg(long)]
        in_graph_neighbors: Option<String>,
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
        #[arg(long, default_value = "f32")]
        element_type: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum CapabilityArg {
    Kv,
    Json,
    Sql,
    Document,
    Graph,
    Codec,
}

impl From<CapabilityArg> for lilia_plugin_api::Capability {
    fn from(arg: CapabilityArg) -> Self {
        match arg {
            CapabilityArg::Kv => Self::Kv,
            CapabilityArg::Json => Self::Json,
            CapabilityArg::Sql => Self::Sql,
            CapabilityArg::Document => Self::Document,
            CapabilityArg::Graph => Self::Graph,
            CapabilityArg::Codec => Self::Codec,
        }
    }
}

#[derive(Debug, Subcommand)]
pub(crate) enum PluginCommand {
    Keygen {
        #[arg(long, default_value = ".")]
        out_dir: PathBuf,
        #[arg(long, default_value = "plugin-signer")]
        name: String,
        #[arg(long)]
        force: bool,
    },
    Pack {
        library: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long)]
        version: String,
        #[arg(long = "capability", value_enum)]
        capabilities: Vec<CapabilityArg>,
        #[arg(long)]
        engine_requirement: Option<String>,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        out_dir: Option<PathBuf>,
    },
    Sign {
        package: PathBuf,
        #[arg(long)]
        key: String,
    },
    Trust {
        #[command(subcommand)]
        command: TrustCommand,
    },
    Verify {
        package: PathBuf,
        #[arg(long = "trusted-key")]
        trusted_keys: Vec<String>,
        #[arg(long)]
        allow_unsigned: bool,
        /// Explicitly enable development-only plugin policy exceptions.
        #[arg(long)]
        development: bool,
    },
    Install {
        package: PathBuf,
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long)]
        force: bool,
        #[arg(long = "trusted-key")]
        trusted_keys: Vec<String>,
        #[arg(long)]
        allow_unsigned: bool,
        /// Explicitly enable development-only plugin policy exceptions.
        #[arg(long)]
        development: bool,
    },
    List {
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long)]
        development: bool,
    },
    Remove {
        installed_name: String,
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long)]
        development: bool,
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

#[derive(Debug, Subcommand)]
pub(crate) enum TrustCommand {
    Add {
        key: String,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long)]
        development: bool,
    },
    List {
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long)]
        development: bool,
    },
    Remove {
        fingerprint: String,
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long)]
        development: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum BitempCommand {
    Put {
        space: String,
        id: String,
        value: String,
        #[arg(long = "from")]
        vt_from: String,
        #[arg(long = "to")]
        vt_to: String,
        #[arg(long = "system-time")]
        system_time: Option<String>,
    },
    Get {
        space: String,
        id: String,
        #[arg(long = "as-of-tt", default_value = "now")]
        as_of_tt: String,
        #[arg(long = "for-vt", default_value = "now")]
        for_vt: String,
    },
    Timeline {
        space: String,
        id: String,
        #[arg(long = "as-of-tt", default_value = "now")]
        as_of_tt: String,
    },
    History {
        space: String,
        id: String,
    },
    Delete {
        space: String,
        id: String,
        #[arg(long = "from")]
        vt_from: Option<String>,
        #[arg(long = "to")]
        vt_to: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum BlobCommand {
    /// Store a local file in the Content Addressable Storage (CAS)
    Put {
        file: PathBuf,
        #[arg(long)]
        mime_type: Option<String>,
    },
    /// Retrieve a blob by its cryptographic hash
    Get {
        hash: String,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Print the local physical file path of a stored blob
    Path { hash: String },
    /// Check whether a blob exists in the CAS store
    Exists { hash: String },
    /// Trigger atomic garbage collection of orphaned blobs
    Gc,
}
