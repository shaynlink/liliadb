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
            DirectionArg::Outgoing => Self::Outgoing,
            DirectionArg::Incoming => Self::Incoming,
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
