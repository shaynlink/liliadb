use std::fs;
use std::path::Path;
#[cfg(not(target_arch = "wasm32"))]
use std::time::{SystemTime, UNIX_EPOCH};

use lilia_sqlite_native::NativeConnection;

use lilia_core::{ErrorCode, LiliaError, Result};

pub(crate) fn migrate(connection: &NativeConnection) -> Result<()> {
    connection.exec(
        "BEGIN IMMEDIATE;
         CREATE TABLE IF NOT EXISTS _lilia_metadata(
           key TEXT PRIMARY KEY NOT NULL,
           value TEXT NOT NULL
         ) STRICT;
         INSERT OR IGNORE INTO _lilia_metadata(key,value) VALUES('format_version','1');
         CREATE TABLE IF NOT EXISTS _lilia_kv(
           namespace TEXT NOT NULL,
           key BLOB NOT NULL,
           value BLOB NOT NULL,
           version INTEGER NOT NULL CHECK(version > 0),
           expires_at_ms INTEGER,
           PRIMARY KEY(namespace,key)
         ) STRICT, WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS _lilia_kv_expiry ON _lilia_kv(expires_at_ms)
           WHERE expires_at_ms IS NOT NULL;
         CREATE TABLE IF NOT EXISTS _lilia_json(
           space TEXT NOT NULL,
           id TEXT NOT NULL,
           value TEXT NOT NULL CHECK(json_valid(value)),
           version INTEGER NOT NULL CHECK(version > 0),
           PRIMARY KEY(space,id)
         ) STRICT, WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS _lilia_graph_nodes(
           graph TEXT NOT NULL,
           id TEXT NOT NULL,
           kind TEXT NOT NULL,
           metadata TEXT NOT NULL CHECK(json_valid(metadata)),
           doc_space TEXT,
           doc_id TEXT,
           PRIMARY KEY(graph,id)
         ) STRICT, WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS _lilia_graph_edges(
           graph TEXT NOT NULL,
           source TEXT NOT NULL,
           target TEXT NOT NULL,
           kind TEXT NOT NULL,
           weight REAL NOT NULL DEFAULT 1.0,
           PRIMARY KEY(graph,source,target,kind)
         ) STRICT, WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS _lilia_graph_edges_target ON _lilia_graph_edges(graph,target);
         CREATE TABLE IF NOT EXISTS _lilia_json_indexes(
           space TEXT NOT NULL,
           index_name TEXT NOT NULL,
           json_path TEXT NOT NULL,
           PRIMARY KEY(space,index_name)
         ) STRICT, WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS _lilia_json_index_entries(
           space TEXT NOT NULL,
           index_name TEXT NOT NULL,
           idx_value TEXT NOT NULL,
           doc_id TEXT NOT NULL,
           PRIMARY KEY(space,index_name,idx_value,doc_id)
         ) STRICT, WITHOUT ROWID;
         CREATE VIRTUAL TABLE IF NOT EXISTS _lilia_fts USING fts5(
           collection UNINDEXED,
           doc_id UNINDEXED,
           content,
           tokenize='unicode61'
         );
         CREATE TABLE IF NOT EXISTS _lilia_vectors(
           collection TEXT NOT NULL,
           id TEXT NOT NULL,
           dim INTEGER NOT NULL,
           element_type INTEGER NOT NULL,
           vector BLOB NOT NULL,
           PRIMARY KEY(collection,id)
         ) STRICT, WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS _lilia_vectors_collection ON _lilia_vectors(collection);
         CREATE TABLE IF NOT EXISTS _lilia_hnsw_meta(
           collection TEXT PRIMARY KEY,
           entry_id TEXT,
           max_level INTEGER NOT NULL,
           m INTEGER NOT NULL,
           ef_construction INTEGER NOT NULL,
           ef_search INTEGER NOT NULL,
           metric TEXT NOT NULL,
           element_type INTEGER NOT NULL,
           dim INTEGER NOT NULL
         ) STRICT, WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS _lilia_hnsw_edges(
           collection TEXT NOT NULL,
           id TEXT NOT NULL,
           level INTEGER NOT NULL,
           neighbors BLOB NOT NULL,
           PRIMARY KEY(collection, id, level)
         ) STRICT, WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS _lilia_timeseries_chunks(
           series_id TEXT NOT NULL,
           resolution_ms INTEGER NOT NULL DEFAULT 0,
           start_time INTEGER NOT NULL,
           end_time INTEGER NOT NULL,
           point_count INTEGER NOT NULL,
           min_val REAL NOT NULL,
           max_val REAL NOT NULL,
           sum_val REAL NOT NULL,
           data BLOB NOT NULL,
           PRIMARY KEY(series_id,resolution_ms,start_time)
         ) STRICT, WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS _lilia_ts_series_end ON _lilia_timeseries_chunks(series_id,resolution_ms,end_time);
         CREATE TABLE IF NOT EXISTS _lilia_timeseries_policies(
           series_pattern TEXT PRIMARY KEY NOT NULL,
           policy_json TEXT NOT NULL
         ) STRICT, WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS _lilia_timeseries_downsample_state(
           series_id TEXT NOT NULL,
           resolution_ms INTEGER NOT NULL,
           last_downsampled_time INTEGER NOT NULL,
           PRIMARY KEY(series_id,resolution_ms)
         ) STRICT, WITHOUT ROWID;
         CREATE VIRTUAL TABLE IF NOT EXISTS _lilia_geofences USING geopoly(
           id,
           name,
           category,
           metadata
         );
         CREATE TABLE IF NOT EXISTS _lilia_gps_points(
           track_id TEXT NOT NULL,
           timestamp INTEGER NOT NULL,
           latitude REAL NOT NULL,
           longitude REAL NOT NULL,
           altitude REAL,
           speed REAL,
           accuracy REAL,
           PRIMARY KEY(track_id,timestamp)
         ) STRICT, WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS _lilia_gps_points_time ON _lilia_gps_points(track_id,timestamp);
         CREATE VIEW IF NOT EXISTS lilia_kv AS
           SELECT namespace, key, lilia_decode_kv(value) AS value, version, expires_at_ms
           FROM _lilia_kv;
         CREATE VIEW IF NOT EXISTS lilia_json AS
           SELECT space, id, lilia_decode_json(value) AS value, version
           FROM _lilia_json;
         CREATE VIEW IF NOT EXISTS lilia_graph_nodes AS
           SELECT graph, id, kind, metadata, doc_space, doc_id
           FROM _lilia_graph_nodes;
         CREATE VIEW IF NOT EXISTS lilia_graph_edges AS
           SELECT graph, source, target, kind, weight
           FROM _lilia_graph_edges;
         CREATE VIEW IF NOT EXISTS lilia_graph_unified AS
           SELECT g.graph, g.id, g.kind, g.metadata,
                  COALESCE(g.doc_space, g.kind) AS doc_space,
                  COALESCE(g.doc_id, g.id) AS doc_id,
                  lilia_decode_json(j.value) AS document
           FROM _lilia_graph_nodes g
           LEFT JOIN _lilia_json j
             ON j.space = COALESCE(g.doc_space, g.kind)
            AND j.id = COALESCE(g.doc_id, g.id);
         CREATE VIEW IF NOT EXISTS lilia_vectors AS
           SELECT collection, id, dim, element_type, lilia_decode_vector(vector) AS vector
           FROM _lilia_vectors;
         CREATE VIEW IF NOT EXISTS lilia_vector_indexes AS
           SELECT collection, entry_id, max_level, m, ef_construction, ef_search, metric, element_type, dim
           FROM _lilia_hnsw_meta;
         CREATE VIEW IF NOT EXISTS lilia_timeseries_chunks AS
           SELECT series_id, resolution_ms, start_time, end_time, point_count, min_val, max_val, sum_val,
                  (sum_val / point_count) AS avg_val, length(data) AS compressed_bytes
           FROM _lilia_timeseries_chunks;
         CREATE VIEW IF NOT EXISTS lilia_timeseries_policies AS
           SELECT series_pattern, policy_json
           FROM _lilia_timeseries_policies;
         CREATE VIEW IF NOT EXISTS lilia_geofences AS
           SELECT rowid, _shape, id, name, category, metadata
           FROM _lilia_geofences;
         CREATE VIEW IF NOT EXISTS lilia_gps_points AS
           SELECT track_id, timestamp, latitude, longitude, altitude, speed, accuracy
           FROM _lilia_gps_points;
         CREATE TABLE IF NOT EXISTS _lilia_json_revisions(
           space TEXT NOT NULL,
           id TEXT NOT NULL,
           version INTEGER NOT NULL CHECK(version > 0),
           timestamp_ms INTEGER NOT NULL,
           delta BLOB NOT NULL,
           is_keyframe INTEGER NOT NULL DEFAULT 0,
           is_deletion INTEGER NOT NULL DEFAULT 0,
           PRIMARY KEY(space,id,version)
         ) STRICT, WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS _lilia_json_rev_time ON _lilia_json_revisions(space,id,timestamp_ms);
         CREATE TABLE IF NOT EXISTS _lilia_undo_log(
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           timestamp_ms INTEGER NOT NULL,
           summary TEXT NOT NULL,
           forward_changeset BLOB NOT NULL,
           reverse_changeset BLOB NOT NULL,
           undone INTEGER NOT NULL DEFAULT 0
         ) STRICT;
         CREATE VIEW IF NOT EXISTS lilia_json_history AS
           SELECT space, id, version, timestamp_ms, is_keyframe, is_deletion
           FROM _lilia_json_revisions;
         COMMIT;",
    )?;

    // Safe schema updates for existing tables
    let _ = connection
        .exec("ALTER TABLE _lilia_graph_edges ADD COLUMN weight REAL NOT NULL DEFAULT 1.0;");
    let _ = connection.exec("ALTER TABLE _lilia_graph_nodes ADD COLUMN doc_space TEXT;");
    let _ = connection.exec("ALTER TABLE _lilia_graph_nodes ADD COLUMN doc_id TEXT;");

    Ok(())
}

pub(crate) fn is_uri_or_memory(path: &Path) -> bool {
    let s = path.to_str().unwrap_or("");
    s == ":memory:" || s.starts_with("file:") || s.contains("?vfs=")
}

pub(crate) fn prepare_parent(path: &Path) -> Result<()> {
    if is_uri_or_memory(path) {
        return Ok(());
    }
    let Some(parent) = path.parent() else {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            "database path has no parent",
            false,
        ));
    };
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        builder.mode(0o700);
        // Existing directories belong to the caller: never change their permissions.
        builder
            .create(parent)
            .map_err(|error| LiliaError::new(ErrorCode::Io, error.to_string(), false))?;
    }
    #[cfg(windows)]
    {
        lilia_core::windows_security::create_dir_all_secure(parent)
            .map_err(|error| LiliaError::new(ErrorCode::Io, error.to_string(), false))?;
    }
    #[cfg(not(any(unix, windows)))]
    {
        fs::create_dir_all(parent)
            .map_err(|error| LiliaError::new(ErrorCode::Io, error.to_string(), false))?;
    }
    lilia_core::sync_directory(parent)
        .map_err(|error| LiliaError::new(ErrorCode::Io, error.to_string(), false))?;
    Ok(())
}

pub(crate) fn secure_database_file(path: &Path) -> Result<()> {
    if is_uri_or_memory(path) {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|error| LiliaError::new(ErrorCode::Io, error.to_string(), false))?;
    }
    #[cfg(windows)]
    {
        lilia_core::windows_security::secure_file(path)
            .map_err(|error| LiliaError::new(ErrorCode::Io, error.to_string(), false))?;
    }
    #[cfg(not(any(unix, windows)))]
    let _ = path;
    Ok(())
}

pub(crate) fn validate_name(field: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 255 || value.chars().any(char::is_control) {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            format!("{field} must contain 1..=255 non-control characters"),
            false,
        ));
    }
    Ok(())
}

pub(crate) fn normalize_limit(limit: u32) -> i64 {
    i64::from(limit.clamp(1, 1_000))
}

pub(crate) fn parse_version(value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|error| {
        LiliaError::new(
            ErrorCode::Storage,
            format!("invalid version {value}: {error}"),
            false,
        )
    })
}

pub(crate) fn store_version(version: u64) -> Result<i64> {
    i64::try_from(version)
        .map_err(|_| LiliaError::new(ErrorCode::Storage, "record version is exhausted", false))
}

pub(crate) fn now_ms() -> i64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| {
                i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
            })
    }
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::now() as i64
    }
}
