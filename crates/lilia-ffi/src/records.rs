//! The records, enums and error of the `UniFFI` API, and their conversions from the engine's types.

use lilia_core::{Direction, ErrorCode, LiliaError, ResourceProfile};

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum LiliaFfiError {
    #[error("Storage error: {0}")]
    Storage(String),
    #[error("Invalid input: {0}")]
    InvalidInput(String),
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("Conflict: {0}")]
    Conflict(String),
    #[error("Constraint violation: {0}")]
    ConstraintViolation(String),
    #[error("Closed: {0}")]
    Closed(String),
    #[error("Timeout: {0}")]
    Timeout(String),
    #[error("Unsupported: {0}")]
    Unsupported(String),
    #[error("Generic error: {0}")]
    Generic(String),
}

impl From<LiliaError> for LiliaFfiError {
    fn from(err: LiliaError) -> Self {
        match err.code {
            ErrorCode::Storage => LiliaFfiError::Storage(err.message),
            ErrorCode::InvalidInput => LiliaFfiError::InvalidInput(err.message),
            ErrorCode::NotFound => LiliaFfiError::NotFound(err.message),
            ErrorCode::Conflict => LiliaFfiError::Conflict(err.message),
            ErrorCode::ConstraintViolation => LiliaFfiError::ConstraintViolation(err.message),
            ErrorCode::Closed => LiliaFfiError::Closed(err.message),
            ErrorCode::Timeout => LiliaFfiError::Timeout(err.message),
            ErrorCode::Unsupported => LiliaFfiError::Unsupported(err.message),
            _ => LiliaFfiError::Generic(err.message),
        }
    }
}

impl From<serde_json::Error> for LiliaFfiError {
    fn from(err: serde_json::Error) -> Self {
        LiliaFfiError::InvalidInput(err.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiResourceProfile {
    Server,
    Mobile,
    Constrained,
}

impl From<FfiResourceProfile> for ResourceProfile {
    fn from(p: FfiResourceProfile) -> Self {
        match p {
            FfiResourceProfile::Server => ResourceProfile::Server,
            FfiResourceProfile::Mobile => ResourceProfile::Mobile,
            FfiResourceProfile::Constrained => ResourceProfile::Constrained,
        }
    }
}

/// What survives a crash or a power loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiDurability {
    /// Every acknowledged commit survives power loss.
    Durable,
    /// The journal stays consistent across power loss; the latest commits may be lost.
    Balanced,
    /// No sync on commit: commits survive a process crash, not power loss.
    Performance,
}

impl FfiDurability {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Durable => "durable",
            Self::Balanced => "balanced",
            Self::Performance => "performance",
        }
    }
}

impl FfiResourceProfile {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Mobile => "mobile",
            Self::Constrained => "constrained",
        }
    }
}

/// How a database opens.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiOpenOptions {
    pub profile: FfiResourceProfile,
    pub durability: FfiDurability,
    /// Keep a tamper-evident ledger of every record write; a database that keeps one must
    /// always be opened with it.
    #[uniffi(default = false)]
    pub ledger: bool,
    /// How long old versions stay readable, in milliseconds; a week when absent, 0 keeps none.
    #[uniffi(default = None)]
    pub history_retention_ms: Option<u64>,
    /// Commits that can be undone; none by default.
    #[uniffi(default = 0)]
    pub undo_depth: u32,
}

impl From<&FfiOpenOptions> for lilia_protocol::OpenOptions {
    fn from(options: &FfiOpenOptions) -> Self {
        Self {
            durability: Some(options.durability.name().to_owned()),
            profile: Some(options.profile.name().to_owned()),
            ledger: options.ledger,
            history_retention_ms: options.history_retention_ms,
            undo_depth: options.undo_depth,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiDirection {
    Outgoing,
    Incoming,
    Both,
}

impl From<FfiDirection> for Direction {
    fn from(d: FfiDirection) -> Self {
        match d {
            FfiDirection::Outgoing => Direction::Outbound,
            FfiDirection::Incoming => Direction::Inbound,
            FfiDirection::Both => Direction::Both,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, uniffi::Record)]
pub struct FfiKvEntry {
    pub namespace: String,
    pub key: Vec<u8>,
    pub value: Vec<u8>,
    pub version: u64,
    pub expires_at_ms: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize, uniffi::Record)]
pub struct FfiJsonEntry {
    pub space: String,
    pub id: String,
    pub value_json: String,
    pub version: u64,
}

/// A secondary index of a JSON space.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiJsonIndexInfo {
    pub space: String,
    pub index_name: String,
    pub json_path: String,
}

impl From<lilia_core::JsonIndexInfo> for FfiJsonIndexInfo {
    fn from(info: lilia_core::JsonIndexInfo) -> Self {
        Self {
            space: info.space,
            index_name: info.index_name,
            json_path: info.json_path,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, uniffi::Record)]
pub struct FfiGraphNode {
    pub graph: String,
    pub id: String,
    pub kind: String,
    pub metadata_json: String,
}

#[derive(Debug, Clone, serde::Serialize, uniffi::Record)]
pub struct FfiGraphEdge {
    pub graph: String,
    pub source: String,
    pub target: String,
    pub kind: String,
}

#[derive(Debug, Clone, serde::Serialize, uniffi::Record)]
pub struct FfiFtsHit {
    pub collection: String,
    pub doc_id: String,
    pub snippet: String,
    pub rank: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, uniffi::Record)]
pub struct FfiFtsSuggestion {
    pub term: String,
    pub distance: u8,
    pub doc_count: u32,
    pub exact_prefix: bool,
}

#[derive(Debug, Clone, serde::Serialize, uniffi::Record)]
pub struct FfiFtsFuzzyHit {
    pub collection: String,
    pub doc_id: String,
    pub snippet: String,
    pub rank: f64,
    pub edit_distance: u8,
    pub matched_terms: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, uniffi::Record)]
pub struct FfiFtsPhoneticHit {
    pub collection: String,
    pub doc_id: String,
    pub snippet: String,
    pub rank: f64,
    pub matched_terms: Vec<String>,
    pub phonetic_codes: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, uniffi::Record)]
pub struct FfiSqlQueryResult {
    pub columns: Vec<String>,
    pub rows_json: Vec<String>,
    pub rows_affected: u64,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, uniffi::Record)]
pub struct FfiTimeSeriesPoint {
    pub timestamp: i64,
    pub value: f64,
}

impl From<lilia_core::TimeSeriesPoint> for FfiTimeSeriesPoint {
    fn from(p: lilia_core::TimeSeriesPoint) -> Self {
        Self {
            timestamp: p.timestamp,
            value: p.value,
        }
    }
}

impl From<FfiTimeSeriesPoint> for lilia_core::TimeSeriesPoint {
    fn from(p: FfiTimeSeriesPoint) -> Self {
        Self {
            timestamp: p.timestamp,
            value: p.value,
        }
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, uniffi::Record)]
pub struct FfiTimeSeriesStats {
    pub min: f64,
    pub max: f64,
    pub avg: f64,
    pub sum: f64,
    pub count: u64,
}

impl From<lilia_core::TimeSeriesStats> for FfiTimeSeriesStats {
    fn from(s: lilia_core::TimeSeriesStats) -> Self {
        Self {
            min: s.min,
            max: s.max,
            avg: s.avg,
            sum: s.sum,
            count: s.count,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Enum)]
pub enum FfiDownsampleAggregation {
    Avg,
    Min,
    Max,
    Sum,
    First,
    Last,
    Count,
}

impl From<lilia_core::DownsampleAggregation> for FfiDownsampleAggregation {
    fn from(a: lilia_core::DownsampleAggregation) -> Self {
        match a {
            lilia_core::DownsampleAggregation::Avg => Self::Avg,
            lilia_core::DownsampleAggregation::Min => Self::Min,
            lilia_core::DownsampleAggregation::Max => Self::Max,
            lilia_core::DownsampleAggregation::Sum => Self::Sum,
            lilia_core::DownsampleAggregation::First => Self::First,
            lilia_core::DownsampleAggregation::Last => Self::Last,
            lilia_core::DownsampleAggregation::Count => Self::Count,
        }
    }
}

impl From<FfiDownsampleAggregation> for lilia_core::DownsampleAggregation {
    fn from(a: FfiDownsampleAggregation) -> Self {
        match a {
            FfiDownsampleAggregation::Avg => Self::Avg,
            FfiDownsampleAggregation::Min => Self::Min,
            FfiDownsampleAggregation::Max => Self::Max,
            FfiDownsampleAggregation::Sum => Self::Sum,
            FfiDownsampleAggregation::First => Self::First,
            FfiDownsampleAggregation::Last => Self::Last,
            FfiDownsampleAggregation::Count => Self::Count,
        }
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, uniffi::Record)]
pub struct FfiTimeSeriesRetentionRule {
    pub resolution_ms: i64,
    pub retention_ms: Option<i64>,
    pub aggregation: FfiDownsampleAggregation,
}

impl From<lilia_core::TimeSeriesRetentionRule> for FfiTimeSeriesRetentionRule {
    fn from(r: lilia_core::TimeSeriesRetentionRule) -> Self {
        Self {
            resolution_ms: r.resolution_ms,
            retention_ms: r.retention_ms,
            aggregation: r.aggregation.into(),
        }
    }
}

impl From<FfiTimeSeriesRetentionRule> for lilia_core::TimeSeriesRetentionRule {
    fn from(r: FfiTimeSeriesRetentionRule) -> Self {
        Self {
            resolution_ms: r.resolution_ms,
            retention_ms: r.retention_ms,
            aggregation: r.aggregation.into(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, uniffi::Record)]
pub struct FfiTimeSeriesLifecyclePolicy {
    pub series_pattern: String,
    pub rules: Vec<FfiTimeSeriesRetentionRule>,
}

impl From<lilia_core::TimeSeriesLifecyclePolicy> for FfiTimeSeriesLifecyclePolicy {
    fn from(p: lilia_core::TimeSeriesLifecyclePolicy) -> Self {
        Self {
            series_pattern: p.series_pattern,
            rules: p.rules.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<FfiTimeSeriesLifecyclePolicy> for lilia_core::TimeSeriesLifecyclePolicy {
    fn from(p: FfiTimeSeriesLifecyclePolicy) -> Self {
        Self {
            series_pattern: p.series_pattern,
            rules: p.rules.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, uniffi::Record)]
pub struct FfiTimeSeriesRetentionStats {
    pub series_processed: u64,
    pub downsampled_points_created: u64,
    pub chunks_pruned: u64,
    pub bytes_reclaimed_approx: u64,
}

impl From<lilia_core::TimeSeriesRetentionStats> for FfiTimeSeriesRetentionStats {
    fn from(s: lilia_core::TimeSeriesRetentionStats) -> Self {
        Self {
            series_processed: s.series_processed,
            downsampled_points_created: s.downsampled_points_created,
            chunks_pruned: s.chunks_pruned,
            bytes_reclaimed_approx: s.bytes_reclaimed_approx,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, uniffi::Record)]
pub struct FfiGpsPoint {
    pub timestamp: i64,
    pub latitude: f64,
    pub longitude: f64,
    pub altitude: Option<f64>,
    pub speed: Option<f64>,
    pub accuracy: Option<f64>,
}

impl From<lilia_core::GpsPoint> for FfiGpsPoint {
    fn from(p: lilia_core::GpsPoint) -> Self {
        Self {
            timestamp: p.timestamp,
            latitude: p.latitude,
            longitude: p.longitude,
            altitude: p.altitude,
            speed: p.speed,
            accuracy: p.accuracy,
        }
    }
}

impl From<FfiGpsPoint> for lilia_core::GpsPoint {
    fn from(p: FfiGpsPoint) -> Self {
        Self {
            timestamp: p.timestamp,
            latitude: p.latitude,
            longitude: p.longitude,
            altitude: p.altitude,
            speed: p.speed,
            accuracy: p.accuracy,
            h3_index: None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, uniffi::Record)]
pub struct FfiGeofenceMatch {
    pub id: String,
    pub name: String,
    pub category: String,
    pub metadata: Option<String>,
}

impl From<lilia_core::GeofenceMatch> for FfiGeofenceMatch {
    fn from(m: lilia_core::GeofenceMatch) -> Self {
        Self {
            id: m.id,
            name: m.name,
            category: m.category,
            metadata: m.metadata,
        }
    }
}
