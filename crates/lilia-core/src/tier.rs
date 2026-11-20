use std::fmt;

use serde::{Deserialize, Serialize};

use crate::Durability;

/// Storage tier determining persistence, memory residency, and write-buffering behavior.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq, Eq))]
#[serde(rename_all = "snake_case")]
pub enum StorageTier {
    /// Durable persistence in SQLite WAL on disk (default).
    #[default]
    Disk,
    /// Purely in-memory RAM storage with zero disk writes and zero WAL fsync.
    Memory,
    /// High-throughput lock-free ring buffer with coalesced micro-batch group commits to SQLite.
    Buffered,
}

impl StorageTier {
    /// Returns the canonical lowercase string label used in SQL views and catalog tables.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disk => "disk",
            Self::Memory => "memory",
            Self::Buffered => "buffered",
        }
    }

    /// Parses a storage tier label (case-insensitive).
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "disk" | "persistent" => Some(Self::Disk),
            "memory" | "ram" | "ephemeral" => Some(Self::Memory),
            "buffered" | "ring" | "ring_buffer" => Some(Self::Buffered),
            _ => None,
        }
    }

    /// Helper used by serde `skip_serializing_if` for compact default wire encoding.
    #[must_use]
    pub const fn is_disk(&self) -> bool {
        matches!(self, Self::Disk)
    }
}

impl fmt::Display for StorageTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Logical `LiliaDB` data model / collection category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq, Eq))]
#[serde(rename_all = "snake_case")]
pub enum CollectionKind {
    Kv,
    Json,
    Graph,
    Fts,
    Vector,
    TimeSeries,
    Crdt,
    Geofence,
    SqlTable,
}

impl CollectionKind {
    /// Returns the canonical lowercase identifier for catalog storage.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Kv => "kv",
            Self::Json => "json",
            Self::Graph => "graph",
            Self::Fts => "fts",
            Self::Vector => "vector",
            Self::TimeSeries => "timeseries",
            Self::Crdt => "crdt",
            Self::Geofence => "geofence",
            Self::SqlTable => "sql_table",
        }
    }

    /// Parses a collection kind name (case-insensitive).
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "kv" => Some(Self::Kv),
            "json" | "doc" | "documents" => Some(Self::Json),
            "graph" => Some(Self::Graph),
            "fts" | "search" => Some(Self::Fts),
            "vector" | "vectors" => Some(Self::Vector),
            "timeseries" | "ts" => Some(Self::TimeSeries),
            "crdt" => Some(Self::Crdt),
            "geofence" | "geo" | "gps" => Some(Self::Geofence),
            "sql" | "table" | "sql_table" => Some(Self::SqlTable),
            _ => None,
        }
    }
}

impl fmt::Display for CollectionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Eviction strategy for bounded ephemeral or RAM-resident collections.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvictionPolicy {
    /// No automatic capacity cap (only optional TTL expiration applies).
    #[default]
    None,
    /// Evict least-recently-accessed items once active entries exceed `max_entries`.
    Lru { max_entries: usize },
    /// Evict oldest inserted items once active entries exceed `max_entries`.
    DropOldest { max_entries: usize },
}

/// Declarative collection specification stored in the tier catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionSpec {
    pub kind: CollectionKind,
    pub name: String,
    pub tier: StorageTier,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<u64>,
    #[serde(default)]
    pub eviction: EvictionPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durability: Option<Durability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compartment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fle_policy: Option<crate::fle::FlePolicy>,
    /// Declarative `CHECK` expression enforced on every JSON document of the collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    /// Background embedding of a text field of every JSON document of the collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_embed: Option<crate::embed::AutoEmbedConfig>,
}

impl CollectionSpec {
    /// Creates a collection specification with default eviction and optional overrides unset.
    #[must_use]
    pub fn new(kind: CollectionKind, name: impl Into<String>, tier: StorageTier) -> Self {
        Self {
            kind,
            name: name.into(),
            tier,
            ttl_ms: None,
            eviction: EvictionPolicy::None,
            durability: None,
            compartment: None,
            fle_policy: None,
            check: None,
            auto_embed: None,
        }
    }

    /// Sets a default TTL (in milliseconds) applied when entries do not provide an explicit expiry.
    #[must_use]
    pub const fn with_ttl_ms(mut self, ttl_ms: u64) -> Self {
        self.ttl_ms = Some(ttl_ms);
        self
    }

    /// Sets the capacity eviction policy for memory-resident entries.
    #[must_use]
    pub const fn with_eviction(mut self, eviction: EvictionPolicy) -> Self {
        self.eviction = eviction;
        self
    }

    /// Sets an optional durability override.
    #[must_use]
    pub const fn with_durability(mut self, durability: Durability) -> Self {
        self.durability = Some(durability);
        self
    }

    /// Sets the multi-tenant / compartment key ID sealing this collection.
    #[must_use]
    pub fn with_compartment(mut self, compartment: impl Into<String>) -> Self {
        self.compartment = Some(compartment.into());
        self
    }

    /// Sets the Field-Level Encryption policy for granular selective properties.
    #[must_use]
    pub fn with_fle_policy(mut self, fle_policy: crate::fle::FlePolicy) -> Self {
        self.fle_policy = Some(fle_policy);
        self
    }
}

/// Alias describing a registered collection entry in introspection results.
pub type CollectionTierEntry = CollectionSpec;

/// Backpressure strategy applied when the lock-free write ring buffer is at capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverflowPolicy {
    /// Wake the group-commit worker and wait up to `timeout_ms` (`None` waits indefinitely).
    Block { timeout_ms: Option<u64> },
    /// Atomically overwrite/evict older queued commands using circular ring semantics.
    DropOldest,
    /// Fail immediately with `ErrorCode::Busy` (retryable).
    Reject,
}

impl Default for OverflowPolicy {
    fn default() -> Self {
        Self::Block {
            timeout_ms: Some(5_000),
        }
    }
}

/// Group-commit acknowledgment mode for buffered writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitMode {
    /// Enqueue in RAM and return immediately; committed asynchronously in micro-batches.
    #[default]
    AsyncBuffered,
    /// Enqueue lock-free and wait together with concurrent writers on one shared WAL commit/fsync.
    GroupSync,
}

/// Configuration for the lock-free write ring buffer and group-commit flusher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RingBufferConfig {
    /// Fixed slot capacity of the lock-free circular queue.
    pub capacity: usize,
    /// Maximum number of queued commands coalesced into one SQLite transaction.
    pub max_batch_size: usize,
    /// Maximum wait interval (milliseconds) before flushing a non-empty partial batch.
    pub flush_interval_ms: u64,
    /// Queue depth threshold that eagerly wakes the background flusher.
    pub watermark_trigger: usize,
    /// Policy executed when producers fill the ring faster than the flusher drains it.
    pub overflow_policy: OverflowPolicy,
    /// Whether queued single-point time-series writes are grouped and compressed per series/chunk.
    pub coalesce_timeseries: bool,
    /// Whether redundant KV set/delete commands for the same key in one batch are collapsed (LWW).
    pub coalesce_kv_writes: bool,
    /// Whether `Block`/`Reject` queues maintain a sequence-stamped RAM overlay for Read-Your-Own-Writes.
    pub enable_read_overlay: bool,
}

impl Default for RingBufferConfig {
    fn default() -> Self {
        Self {
            capacity: 65_536,
            max_batch_size: 2_048,
            flush_interval_ms: 10,
            watermark_trigger: 1_024,
            overflow_policy: OverflowPolicy::default(),
            coalesce_timeseries: true,
            coalesce_kv_writes: false,
            enable_read_overlay: true,
        }
    }
}

impl RingBufferConfig {
    /// Returns a normalized copy with safe non-zero bounds.
    #[must_use]
    pub fn normalized(&self) -> Self {
        let capacity = self.capacity.max(16);
        let max_batch_size = self.max_batch_size.clamp(1, capacity);
        let watermark_trigger = self.watermark_trigger.clamp(1, capacity);
        Self {
            capacity,
            max_batch_size,
            flush_interval_ms: self.flush_interval_ms.max(1),
            watermark_trigger,
            overflow_policy: self.overflow_policy,
            coalesce_timeseries: self.coalesce_timeseries,
            coalesce_kv_writes: self.coalesce_kv_writes,
            enable_read_overlay: self.enable_read_overlay,
        }
    }
}

/// Live observability counters for the lock-free write ring buffer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RingBufferStats {
    pub capacity: usize,
    pub queued_len: usize,
    pub enqueued_total: u64,
    pub committed_total: u64,
    pub batches_committed: u64,
    pub coalesced_ts_points: u64,
    pub coalesced_kv_ops: u64,
    pub dropped_oldest_total: u64,
    pub rejected_total: u64,
    pub flush_errors_total: u64,
    pub overlay_entries: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub last_commit_latency_us: u64,
}

/// Live observability snapshot for the pure-RAM `MemEngine` plane.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryTierStats {
    pub kv_entries: usize,
    pub json_entries: usize,
    pub timeseries_points: usize,
    pub graph_nodes: usize,
    pub graph_edges: usize,
    pub vector_entries: usize,
    pub crdt_entries: usize,
    pub estimated_bytes: usize,
    pub ttl_evictions: u64,
    pub lru_evictions: u64,
}
