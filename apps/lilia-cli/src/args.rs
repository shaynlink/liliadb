use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use lilia_protocol::OpenOptions;

pub(crate) mod models;

use models::{
    BitempCommand, CrdtCommand, FtsCommand, GraphCommand, HybridCommand, JsonCommand, KvCommand,
    LedgerCommand, VectorCommand,
};

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
