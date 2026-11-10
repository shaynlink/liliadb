#![allow(unsafe_code)]

uniffi::setup_scaffolding!();

pub mod c_abi;
pub use c_abi::*;

use std::sync::Arc;

use lilia_core::{Aes256GcmCodec, BatchOperation, Direction, ErrorCode, LiliaError};
use lilia_storage_sqlite::{Database, DatabaseOptions, ResourceProfile};

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
    #[error("Crypto error: {0}")]
    Crypto(String),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiDirection {
    Outgoing,
    Incoming,
    Both,
}

impl From<FfiDirection> for Direction {
    fn from(d: FfiDirection) -> Self {
        match d {
            FfiDirection::Outgoing => Direction::Outgoing,
            FfiDirection::Incoming => Direction::Incoming,
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

#[uniffi::export]
pub fn ts_compress_points(points: Vec<FfiTimeSeriesPoint>) -> Result<Vec<u8>, LiliaFfiError> {
    let core_points: Vec<lilia_core::TimeSeriesPoint> =
        points.into_iter().map(Into::into).collect();
    let comp = lilia_core::compress_timeseries(&core_points)?;
    Ok(comp)
}

#[uniffi::export]
pub fn ts_decompress_points(bytes: Vec<u8>) -> Result<Vec<FfiTimeSeriesPoint>, LiliaFfiError> {
    let core_points = lilia_core::decompress_timeseries(&bytes)?;
    Ok(core_points.into_iter().map(Into::into).collect())
}

#[uniffi::export]
pub fn geo_haversine_distance(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    lilia_core::haversine_distance_meters(lat1, lon1, lat2, lon2)
}

#[uniffi::export]
pub fn geo_create_circle_polygon(
    center_lon: f64,
    center_lat: f64,
    radius_meters: f64,
    n_vertices: u32,
) -> String {
    lilia_core::circle_to_geopoly_json(center_lon, center_lat, radius_meters, n_vertices as usize)
}

#[derive(Debug, uniffi::Object)]
pub struct LiliaDatabase {
    inner: Arc<Database>,
}

#[uniffi::export]
impl LiliaDatabase {
    /// Open a database with a specific resource profile and optional encryption key.
    ///
    /// When `encryption_key` (32 bytes / 256 bits) is provided, transparent authenticated
    /// AES-256-GCM encryption is activated on the database file.
    #[uniffi::constructor]
    pub fn open(
        path: String,
        profile: FfiResourceProfile,
        encryption_key: Option<Vec<u8>>,
    ) -> Result<Arc<Self>, LiliaFfiError> {
        let mut opts = match profile {
            FfiResourceProfile::Server => DatabaseOptions::durable(&path),
            FfiResourceProfile::Mobile => DatabaseOptions::mobile(&path),
            FfiResourceProfile::Constrained => DatabaseOptions::constrained(&path),
        };

        if let Some(key) = encryption_key {
            if key.len() != 32 {
                return Err(LiliaFfiError::Crypto(
                    "Encryption key must be exactly 32 bytes (256 bits)".into(),
                ));
            }
            let mut key_arr = [0u8; 32];
            key_arr.copy_from_slice(&key);
            opts = opts.with_codec(Arc::new(Aes256GcmCodec::from_key(key_arr)));
        }

        let db = Database::open(opts)?;
        Ok(Arc::new(Self {
            inner: Arc::new(db),
        }))
    }

    /// Open with a passphrase using PBKDF2-HMAC-SHA256 key derivation.
    #[uniffi::constructor]
    pub fn open_with_passphrase(
        path: String,
        profile: FfiResourceProfile,
        passphrase: String,
    ) -> Result<Arc<Self>, LiliaFfiError> {
        let mut opts = match profile {
            FfiResourceProfile::Server => DatabaseOptions::durable(&path),
            FfiResourceProfile::Mobile => DatabaseOptions::mobile(&path),
            FfiResourceProfile::Constrained => DatabaseOptions::constrained(&path),
        };
        opts = opts.with_codec(Arc::new(Aes256GcmCodec::from_passphrase(&passphrase)));
        let db = Database::open(opts)?;
        Ok(Arc::new(Self {
            inner: Arc::new(db),
        }))
    }

    /// Mobile lifecycle helper: flushes the WAL and commits any pending pages before
    /// the mobile OS suspends the app process.
    pub fn prepare_for_background(&self) -> Result<(), LiliaFfiError> {
        self.inner.checkpoint()?;
        Ok(())
    }

    // ==========================================
    // KEY / VALUE OPERATIONS (Sync & Async)
    // ==========================================

    pub fn kv_get_sync(
        &self,
        namespace: String,
        key: Vec<u8>,
    ) -> Result<Option<FfiKvEntry>, LiliaFfiError> {
        let entry = self.inner.kv_get(&namespace, &key)?;
        Ok(entry.map(|e| FfiKvEntry {
            namespace: e.namespace,
            key: e.key,
            value: e.value,
            version: e.version,
            expires_at_ms: e.expires_at_ms,
        }))
    }

    pub async fn kv_get(
        &self,
        namespace: String,
        key: Vec<u8>,
    ) -> Result<Option<FfiKvEntry>, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let entry = db.kv_get(&namespace, &key)?;
            Ok(entry.map(|e| FfiKvEntry {
                namespace: e.namespace,
                key: e.key,
                value: e.value,
                version: e.version,
                expires_at_ms: e.expires_at_ms,
            }))
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn kv_set_sync(
        &self,
        namespace: String,
        key: Vec<u8>,
        value: Vec<u8>,
        expires_at_ms: Option<i64>,
    ) -> Result<u64, LiliaFfiError> {
        let results = self.inner.batch(&[BatchOperation::KvSet {
            namespace,
            key,
            value,
            if_version: None,
            expires_at_ms,
        }])?;
        Ok(results.first().and_then(|r| r.version).unwrap_or(0))
    }

    pub async fn kv_set(
        &self,
        namespace: String,
        key: Vec<u8>,
        value: Vec<u8>,
        expires_at_ms: Option<i64>,
    ) -> Result<u64, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let results = db.batch(&[BatchOperation::KvSet {
                namespace,
                key,
                value,
                if_version: None,
                expires_at_ms,
            }])?;
            Ok(results.first().and_then(|r| r.version).unwrap_or(0))
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn kv_delete_sync(&self, namespace: String, key: Vec<u8>) -> Result<bool, LiliaFfiError> {
        let results = self.inner.batch(&[BatchOperation::KvDelete {
            namespace,
            key,
            if_version: None,
        }])?;
        Ok(results.first().is_some_and(|r| r.deleted))
    }

    pub async fn kv_delete(&self, namespace: String, key: Vec<u8>) -> Result<bool, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let results = db.batch(&[BatchOperation::KvDelete {
                namespace,
                key,
                if_version: None,
            }])?;
            Ok(results.first().is_some_and(|r| r.deleted))
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn kv_scan_sync(
        &self,
        namespace: String,
        after: Option<Vec<u8>>,
        limit: u32,
    ) -> Result<Vec<FfiKvEntry>, LiliaFfiError> {
        let entries = self.inner.kv_scan(&namespace, after.as_deref(), limit)?;
        Ok(entries
            .into_iter()
            .map(|e| FfiKvEntry {
                namespace: e.namespace,
                key: e.key,
                value: e.value,
                version: e.version,
                expires_at_ms: e.expires_at_ms,
            })
            .collect())
    }

    pub async fn kv_scan(
        &self,
        namespace: String,
        after: Option<Vec<u8>>,
        limit: u32,
    ) -> Result<Vec<FfiKvEntry>, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let entries = db.kv_scan(&namespace, after.as_deref(), limit)?;
            Ok(entries
                .into_iter()
                .map(|e| FfiKvEntry {
                    namespace: e.namespace,
                    key: e.key,
                    value: e.value,
                    version: e.version,
                    expires_at_ms: e.expires_at_ms,
                })
                .collect())
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    // ==========================================
    // JSON DOCUMENT OPERATIONS (Sync & Async)
    // ==========================================

    pub fn json_get_sync(
        &self,
        space: String,
        id: String,
    ) -> Result<Option<FfiJsonEntry>, LiliaFfiError> {
        let entry = self.inner.json_get(&space, &id)?;
        Ok(entry.map(|e| FfiJsonEntry {
            space: e.space,
            id: e.id,
            value_json: e.value.to_string(),
            version: e.version,
        }))
    }

    pub async fn json_get(
        &self,
        space: String,
        id: String,
    ) -> Result<Option<FfiJsonEntry>, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let entry = db.json_get(&space, &id)?;
            Ok(entry.map(|e| FfiJsonEntry {
                space: e.space,
                id: e.id,
                value_json: e.value.to_string(),
                version: e.version,
            }))
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn json_put_sync(
        &self,
        space: String,
        id: String,
        value_json: String,
    ) -> Result<u64, LiliaFfiError> {
        let parsed: serde_json::Value = serde_json::from_str(&value_json)?;
        let results = self.inner.batch(&[BatchOperation::JsonPut {
            space,
            id,
            value: parsed,
            if_version: None,
        }])?;
        Ok(results.first().and_then(|r| r.version).unwrap_or(0))
    }

    pub async fn json_put(
        &self,
        space: String,
        id: String,
        value_json: String,
    ) -> Result<u64, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let parsed: serde_json::Value = serde_json::from_str(&value_json)?;
            let results = db.batch(&[BatchOperation::JsonPut {
                space,
                id,
                value: parsed,
                if_version: None,
            }])?;
            Ok(results.first().and_then(|r| r.version).unwrap_or(0))
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn json_delete_sync(&self, space: String, id: String) -> Result<bool, LiliaFfiError> {
        let results = self.inner.batch(&[BatchOperation::JsonDelete {
            space,
            id,
            if_version: None,
        }])?;
        Ok(results.first().is_some_and(|r| r.deleted))
    }

    pub async fn json_delete(&self, space: String, id: String) -> Result<bool, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let results = db.batch(&[BatchOperation::JsonDelete {
                space,
                id,
                if_version: None,
            }])?;
            Ok(results.first().is_some_and(|r| r.deleted))
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn json_find_sync(
        &self,
        space: String,
        query_json: String,
        after_id: Option<String>,
        limit: u32,
    ) -> Result<Vec<FfiJsonEntry>, LiliaFfiError> {
        let parsed: serde_json::Value = serde_json::from_str(&query_json)?;
        let map = parsed.as_object().ok_or_else(|| {
            LiliaFfiError::InvalidInput("query_json must be a JSON object".into())
        })?;
        let entries = self
            .inner
            .json_find(&space, map, after_id.as_deref(), limit)?;
        Ok(entries
            .into_iter()
            .map(|e| FfiJsonEntry {
                space: e.space,
                id: e.id,
                value_json: e.value.to_string(),
                version: e.version,
            })
            .collect())
    }

    pub async fn json_find(
        &self,
        space: String,
        query_json: String,
        after_id: Option<String>,
        limit: u32,
    ) -> Result<Vec<FfiJsonEntry>, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let parsed: serde_json::Value = serde_json::from_str(&query_json)?;
            let map = parsed.as_object().ok_or_else(|| {
                LiliaFfiError::InvalidInput("query_json must be a JSON object".into())
            })?;
            let entries = db.json_find(&space, map, after_id.as_deref(), limit)?;
            Ok(entries
                .into_iter()
                .map(|e| FfiJsonEntry {
                    space: e.space,
                    id: e.id,
                    value_json: e.value.to_string(),
                    version: e.version,
                })
                .collect())
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    // ==========================================
    // GRAPH OPERATIONS (Sync & Async)
    // ==========================================

    pub fn graph_set_node_sync(
        &self,
        graph: String,
        id: String,
        kind: String,
        metadata_json: String,
    ) -> Result<(), LiliaFfiError> {
        let meta: serde_json::Value = serde_json::from_str(&metadata_json)?;
        self.inner.graph_set_node(&graph, &id, &kind, &meta)?;
        Ok(())
    }

    pub async fn graph_set_node(
        &self,
        graph: String,
        id: String,
        kind: String,
        metadata_json: String,
    ) -> Result<(), LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let meta: serde_json::Value = serde_json::from_str(&metadata_json)?;
            db.graph_set_node(&graph, &id, &kind, &meta)?;
            Ok(())
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn graph_get_node_sync(
        &self,
        graph: String,
        id: String,
    ) -> Result<Option<FfiGraphNode>, LiliaFfiError> {
        let node = self.inner.graph_get_node(&graph, &id)?;
        Ok(node.map(|n| FfiGraphNode {
            graph: n.graph,
            id: n.id,
            kind: n.kind,
            metadata_json: n.metadata.to_string(),
        }))
    }

    pub async fn graph_get_node(
        &self,
        graph: String,
        id: String,
    ) -> Result<Option<FfiGraphNode>, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let node = db.graph_get_node(&graph, &id)?;
            Ok(node.map(|n| FfiGraphNode {
                graph: n.graph,
                id: n.id,
                kind: n.kind,
                metadata_json: n.metadata.to_string(),
            }))
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn graph_delete_node_sync(&self, graph: String, id: String) -> Result<bool, LiliaFfiError> {
        let res = self.inner.graph_delete_node(&graph, &id)?;
        Ok(res)
    }

    pub async fn graph_delete_node(
        &self,
        graph: String,
        id: String,
    ) -> Result<bool, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let res = db.graph_delete_node(&graph, &id)?;
            Ok(res)
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn graph_add_edge_sync(
        &self,
        graph: String,
        source: String,
        target: String,
        kind: String,
    ) -> Result<(), LiliaFfiError> {
        self.inner.graph_add_edge(&graph, &source, &target, &kind)?;
        Ok(())
    }

    pub async fn graph_add_edge(
        &self,
        graph: String,
        source: String,
        target: String,
        kind: String,
    ) -> Result<(), LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            db.graph_add_edge(&graph, &source, &target, &kind)?;
            Ok(())
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn graph_remove_edge_sync(
        &self,
        graph: String,
        source: String,
        target: String,
        kind: String,
    ) -> Result<bool, LiliaFfiError> {
        let res = self
            .inner
            .graph_remove_edge(&graph, &source, &target, &kind)?;
        Ok(res)
    }

    pub async fn graph_remove_edge(
        &self,
        graph: String,
        source: String,
        target: String,
        kind: String,
    ) -> Result<bool, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let res = db.graph_remove_edge(&graph, &source, &target, &kind)?;
            Ok(res)
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn graph_neighbors_sync(
        &self,
        graph: String,
        node_id: String,
        direction: FfiDirection,
        kind_filter: Option<String>,
    ) -> Result<Vec<String>, LiliaFfiError> {
        let neighbors = self.inner.graph_neighbors(
            &graph,
            &node_id,
            direction.into(),
            kind_filter.as_deref(),
        )?;
        Ok(neighbors)
    }

    pub async fn graph_neighbors(
        &self,
        graph: String,
        node_id: String,
        direction: FfiDirection,
        kind_filter: Option<String>,
    ) -> Result<Vec<String>, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let neighbors =
                db.graph_neighbors(&graph, &node_id, direction.into(), kind_filter.as_deref())?;
            Ok(neighbors)
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn graph_reachability_sync(
        &self,
        graph: String,
        start_nodes: Vec<String>,
        direction: FfiDirection,
    ) -> Result<Vec<String>, LiliaFfiError> {
        let res = self
            .inner
            .graph_reachability(&graph, &start_nodes, direction.into())?;
        Ok(res)
    }

    pub async fn graph_reachability(
        &self,
        graph: String,
        start_nodes: Vec<String>,
        direction: FfiDirection,
    ) -> Result<Vec<String>, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let res = db.graph_reachability(&graph, &start_nodes, direction.into())?;
            Ok(res)
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    // ==========================================
    // FULL-TEXT SEARCH (FTS5) (Sync & Async)
    // ==========================================

    pub fn fts_index_sync(
        &self,
        collection: String,
        doc_id: String,
        content: String,
    ) -> Result<(), LiliaFfiError> {
        self.inner.fts_index(&collection, &doc_id, &content)?;
        Ok(())
    }

    pub async fn fts_index(
        &self,
        collection: String,
        doc_id: String,
        content: String,
    ) -> Result<(), LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            db.fts_index(&collection, &doc_id, &content)?;
            Ok(())
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn fts_delete_sync(
        &self,
        collection: String,
        doc_id: String,
    ) -> Result<bool, LiliaFfiError> {
        let res = self.inner.fts_delete(&collection, &doc_id)?;
        Ok(res)
    }

    pub async fn fts_delete(
        &self,
        collection: String,
        doc_id: String,
    ) -> Result<bool, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let res = db.fts_delete(&collection, &doc_id)?;
            Ok(res)
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn fts_search_sync(
        &self,
        collection: String,
        query: String,
        limit: u32,
    ) -> Result<Vec<FfiFtsHit>, LiliaFfiError> {
        let hits = self.inner.fts_search(&collection, &query, limit)?;
        Ok(hits
            .into_iter()
            .map(|h| FfiFtsHit {
                collection: h.collection,
                doc_id: h.doc_id,
                snippet: h.snippet,
                rank: h.rank,
            })
            .collect())
    }

    pub async fn fts_search(
        &self,
        collection: String,
        query: String,
        limit: u32,
    ) -> Result<Vec<FfiFtsHit>, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let hits = db.fts_search(&collection, &query, limit)?;
            Ok(hits
                .into_iter()
                .map(|h| FfiFtsHit {
                    collection: h.collection,
                    doc_id: h.doc_id,
                    snippet: h.snippet,
                    rank: h.rank,
                })
                .collect())
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    // ==========================================
    // SQL ANALYTICS (Sync & Async)
    // ==========================================

    pub fn sql_query_sync(
        &self,
        sql: String,
        params_json: Option<String>,
    ) -> Result<FfiSqlQueryResult, LiliaFfiError> {
        let params: Vec<serde_json::Value> = match params_json {
            Some(json_str) => serde_json::from_str(&json_str)?,
            None => Vec::new(),
        };
        let res = self.inner.sql_query(&sql, &params)?;
        let rows_json = res
            .rows
            .into_iter()
            .map(|row| serde_json::to_string(&row).unwrap_or_else(|_| "[]".into()))
            .collect();
        Ok(FfiSqlQueryResult {
            columns: res.columns,
            rows_json,
            rows_affected: res.rows_affected,
        })
    }

    pub async fn sql_query(
        &self,
        sql: String,
        params_json: Option<String>,
    ) -> Result<FfiSqlQueryResult, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let params: Vec<serde_json::Value> = match params_json {
                Some(json_str) => serde_json::from_str(&json_str)?,
                None => Vec::new(),
            };
            let res = db.sql_query(&sql, &params)?;
            let rows_json = res
                .rows
                .into_iter()
                .map(|row| serde_json::to_string(&row).unwrap_or_else(|_| "[]".into()))
                .collect();
            Ok(FfiSqlQueryResult {
                columns: res.columns,
                rows_json,
                rows_affected: res.rows_affected,
            })
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    // ==========================================
    // MAINTENANCE & CHECKS (Sync & Async)
    // ==========================================

    pub fn checkpoint_sync(&self) -> Result<(), LiliaFfiError> {
        self.inner.checkpoint()?;
        Ok(())
    }

    pub async fn checkpoint(&self) -> Result<(), LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            db.checkpoint()?;
            Ok(())
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn backup_sync(&self, destination_path: String) -> Result<(), LiliaFfiError> {
        self.inner.backup(destination_path)?;
        Ok(())
    }

    pub async fn backup(&self, destination_path: String) -> Result<(), LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            db.backup(destination_path)?;
            Ok(())
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn quick_check_sync(&self) -> Result<bool, LiliaFfiError> {
        let ok = self.inner.quick_check()?;
        Ok(ok)
    }

    pub async fn quick_check(&self) -> Result<bool, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let ok = db.quick_check()?;
            Ok(ok)
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    // ==========================================
    // TIME-SERIES HEALTH SENSORS (Sync & Async)
    // ==========================================

    pub fn ts_append_points_sync(
        &self,
        series_id: String,
        points: Vec<FfiTimeSeriesPoint>,
    ) -> Result<(), LiliaFfiError> {
        let core_points: Vec<lilia_core::TimeSeriesPoint> =
            points.into_iter().map(Into::into).collect();
        self.inner.ts_append_points(&series_id, &core_points)?;
        Ok(())
    }

    pub async fn ts_append_points(
        &self,
        series_id: String,
        points: Vec<FfiTimeSeriesPoint>,
    ) -> Result<(), LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let core_points: Vec<lilia_core::TimeSeriesPoint> =
                points.into_iter().map(Into::into).collect();
            db.ts_append_points(&series_id, &core_points)?;
            Ok(())
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn ts_query_range_sync(
        &self,
        series_id: String,
        start_time: i64,
        end_time: i64,
    ) -> Result<Vec<FfiTimeSeriesPoint>, LiliaFfiError> {
        let pts = self
            .inner
            .ts_query_range(&series_id, start_time, end_time)?;
        Ok(pts.into_iter().map(Into::into).collect())
    }

    pub async fn ts_query_range(
        &self,
        series_id: String,
        start_time: i64,
        end_time: i64,
    ) -> Result<Vec<FfiTimeSeriesPoint>, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let pts = db.ts_query_range(&series_id, start_time, end_time)?;
            Ok(pts.into_iter().map(Into::into).collect())
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn ts_query_stats_sync(
        &self,
        series_id: String,
        start_time: i64,
        end_time: i64,
    ) -> Result<Option<FfiTimeSeriesStats>, LiliaFfiError> {
        let stats = self
            .inner
            .ts_query_stats(&series_id, start_time, end_time)?;
        Ok(stats.map(Into::into))
    }

    pub async fn ts_query_stats(
        &self,
        series_id: String,
        start_time: i64,
        end_time: i64,
    ) -> Result<Option<FfiTimeSeriesStats>, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let stats = db.ts_query_stats(&series_id, start_time, end_time)?;
            Ok(stats.map(Into::into))
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn ts_query_auto_sync(
        &self,
        series_id: String,
        start_time: i64,
        end_time: i64,
        max_points: u32,
    ) -> Result<Vec<FfiTimeSeriesPoint>, LiliaFfiError> {
        let pts =
            self.inner
                .ts_query_auto(&series_id, start_time, end_time, max_points as usize)?;
        Ok(pts.into_iter().map(Into::into).collect())
    }

    pub async fn ts_query_auto(
        &self,
        series_id: String,
        start_time: i64,
        end_time: i64,
        max_points: u32,
    ) -> Result<Vec<FfiTimeSeriesPoint>, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let pts = db.ts_query_auto(&series_id, start_time, end_time, max_points as usize)?;
            Ok(pts.into_iter().map(Into::into).collect())
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn ts_query_range_with_resolution_sync(
        &self,
        series_id: String,
        resolution_ms: i64,
        start_time: i64,
        end_time: i64,
    ) -> Result<Vec<FfiTimeSeriesPoint>, LiliaFfiError> {
        let pts = self.inner.ts_query_range_with_resolution(
            &series_id,
            resolution_ms,
            start_time,
            end_time,
        )?;
        Ok(pts.into_iter().map(Into::into).collect())
    }

    pub async fn ts_query_range_with_resolution(
        &self,
        series_id: String,
        resolution_ms: i64,
        start_time: i64,
        end_time: i64,
    ) -> Result<Vec<FfiTimeSeriesPoint>, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let pts =
                db.ts_query_range_with_resolution(&series_id, resolution_ms, start_time, end_time)?;
            Ok(pts.into_iter().map(Into::into).collect())
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn ts_set_policy_sync(
        &self,
        policy: FfiTimeSeriesLifecyclePolicy,
    ) -> Result<(), LiliaFfiError> {
        let core_policy: lilia_core::TimeSeriesLifecyclePolicy = policy.into();
        self.inner.ts_set_policy(&core_policy)?;
        Ok(())
    }

    pub async fn ts_set_policy(
        &self,
        policy: FfiTimeSeriesLifecyclePolicy,
    ) -> Result<(), LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let core_policy: lilia_core::TimeSeriesLifecyclePolicy = policy.into();
            db.ts_set_policy(&core_policy)?;
            Ok(())
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn ts_get_policy_sync(
        &self,
        series_pattern: String,
    ) -> Result<Option<FfiTimeSeriesLifecyclePolicy>, LiliaFfiError> {
        let policy = self.inner.ts_get_policy(&series_pattern)?;
        Ok(policy.map(Into::into))
    }

    pub async fn ts_get_policy(
        &self,
        series_pattern: String,
    ) -> Result<Option<FfiTimeSeriesLifecyclePolicy>, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let policy = db.ts_get_policy(&series_pattern)?;
            Ok(policy.map(Into::into))
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn ts_list_policies_sync(
        &self,
    ) -> Result<Vec<FfiTimeSeriesLifecyclePolicy>, LiliaFfiError> {
        let policies = self.inner.ts_list_policies()?;
        Ok(policies.into_iter().map(Into::into).collect())
    }

    pub async fn ts_list_policies(
        &self,
    ) -> Result<Vec<FfiTimeSeriesLifecyclePolicy>, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let policies = db.ts_list_policies()?;
            Ok(policies.into_iter().map(Into::into).collect())
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn ts_delete_policy_sync(&self, series_pattern: String) -> Result<bool, LiliaFfiError> {
        let deleted = self.inner.ts_delete_policy(&series_pattern)?;
        Ok(deleted)
    }

    pub async fn ts_delete_policy(&self, series_pattern: String) -> Result<bool, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let deleted = db.ts_delete_policy(&series_pattern)?;
            Ok(deleted)
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn ts_apply_retention_sync(&self) -> Result<FfiTimeSeriesRetentionStats, LiliaFfiError> {
        let stats = self.inner.ts_apply_retention()?;
        Ok(stats.into())
    }

    pub async fn ts_apply_retention(&self) -> Result<FfiTimeSeriesRetentionStats, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let stats = db.ts_apply_retention()?;
            Ok(stats.into())
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    // ==========================================
    // GEOFENCING & GPS TRACKS (Sync & Async)
    // ==========================================

    pub fn geo_add_fence_sync(
        &self,
        id: String,
        name: String,
        category: String,
        polygon_json: String,
        metadata: Option<String>,
    ) -> Result<(), LiliaFfiError> {
        self.inner
            .geo_add_fence(&id, &name, &category, &polygon_json, metadata.as_deref())?;
        Ok(())
    }

    pub async fn geo_add_fence(
        &self,
        id: String,
        name: String,
        category: String,
        polygon_json: String,
        metadata: Option<String>,
    ) -> Result<(), LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            db.geo_add_fence(&id, &name, &category, &polygon_json, metadata.as_deref())?;
            Ok(())
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn geo_remove_fence_sync(&self, id: String) -> Result<bool, LiliaFfiError> {
        let ok = self.inner.geo_remove_fence(&id)?;
        Ok(ok)
    }

    pub async fn geo_remove_fence(&self, id: String) -> Result<bool, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let ok = db.geo_remove_fence(&id)?;
            Ok(ok)
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn geo_contains_point_sync(
        &self,
        polygon_json: String,
        lon: f64,
        lat: f64,
    ) -> Result<bool, LiliaFfiError> {
        let inside = self.inner.geo_contains_point(&polygon_json, lon, lat)?;
        Ok(inside)
    }

    pub async fn geo_contains_point(
        &self,
        polygon_json: String,
        lon: f64,
        lat: f64,
    ) -> Result<bool, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let inside = db.geo_contains_point(&polygon_json, lon, lat)?;
            Ok(inside)
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn geo_find_fences_for_point_sync(
        &self,
        lon: f64,
        lat: f64,
    ) -> Result<Vec<FfiGeofenceMatch>, LiliaFfiError> {
        let matches = self.inner.geo_find_fences_for_point(lon, lat)?;
        Ok(matches.into_iter().map(Into::into).collect())
    }

    pub async fn geo_find_fences_for_point(
        &self,
        lon: f64,
        lat: f64,
    ) -> Result<Vec<FfiGeofenceMatch>, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let matches = db.geo_find_fences_for_point(lon, lat)?;
            Ok(matches.into_iter().map(Into::into).collect())
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn geo_record_gps_points_sync(
        &self,
        track_id: String,
        points: Vec<FfiGpsPoint>,
    ) -> Result<Vec<FfiGeofenceMatch>, LiliaFfiError> {
        let core_pts: Vec<lilia_core::GpsPoint> = points.into_iter().map(Into::into).collect();
        let matches = self.inner.geo_record_gps_points(&track_id, &core_pts)?;
        Ok(matches.into_iter().map(Into::into).collect())
    }

    pub async fn geo_record_gps_points(
        &self,
        track_id: String,
        points: Vec<FfiGpsPoint>,
    ) -> Result<Vec<FfiGeofenceMatch>, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let core_pts: Vec<lilia_core::GpsPoint> = points.into_iter().map(Into::into).collect();
            let matches = db.geo_record_gps_points(&track_id, &core_pts)?;
            Ok(matches.into_iter().map(Into::into).collect())
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn geo_get_track_points_sync(
        &self,
        track_id: String,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> Result<Vec<FfiGpsPoint>, LiliaFfiError> {
        let pts = self
            .inner
            .geo_get_track_points(&track_id, start_time, end_time)?;
        Ok(pts.into_iter().map(Into::into).collect())
    }

    pub async fn geo_get_track_points(
        &self,
        track_id: String,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> Result<Vec<FfiGpsPoint>, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let pts = db.geo_get_track_points(&track_id, start_time, end_time)?;
            Ok(pts.into_iter().map(Into::into).collect())
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn geo_track_distance_sync(&self, track_id: String) -> Result<f64, LiliaFfiError> {
        let dist = self.inner.geo_track_distance_meters(&track_id)?;
        Ok(dist)
    }

    pub async fn geo_track_distance(&self, track_id: String) -> Result<f64, LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let dist = db.geo_track_distance_meters(&track_id)?;
            Ok(dist)
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }

    pub fn close_sync(&self, timeout_ms: u64) -> Result<(), LiliaFfiError> {
        self.inner
            .close(std::time::Duration::from_millis(timeout_ms))?;
        Ok(())
    }

    pub async fn close(&self, timeout_ms: u64) -> Result<(), LiliaFfiError> {
        let db = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            db.close(std::time::Duration::from_millis(timeout_ms))?;
            Ok(())
        })
        .await
        .map_err(|e| LiliaFfiError::Generic(e.to_string()))?
    }
}
