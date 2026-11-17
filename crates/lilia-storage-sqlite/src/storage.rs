use std::fs;
use std::path::Path;
#[cfg(not(target_arch = "wasm32"))]
use std::time::{SystemTime, UNIX_EPOCH};

use lilia_sqlite_native::NativeConnection;

use lilia_core::{ErrorCode, LiliaError, Result};

#[allow(clippy::too_many_lines)] // one sequential procedure over shared state; splitting it would only thread that state through helpers
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
           value ANY NOT NULL,
           format INTEGER NOT NULL DEFAULT 0,
           dict_id INTEGER NOT NULL DEFAULT 0,
           version INTEGER NOT NULL CHECK(version > 0),
           PRIMARY KEY(space,id)
         ) STRICT, WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS _lilia_json_dictionaries(
           space TEXT NOT NULL,
           dict_id INTEGER NOT NULL,
           dict_data BLOB NOT NULL,
           sample_count INTEGER NOT NULL,
           dict_size INTEGER NOT NULL,
           created_at_ms INTEGER NOT NULL,
           PRIMARY KEY(space,dict_id)
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
         CREATE INDEX IF NOT EXISTS _lilia_graph_nodes_doc ON _lilia_graph_nodes(doc_space, doc_id)
           WHERE doc_space IS NOT NULL;
         CREATE TABLE IF NOT EXISTS _lilia_security_policies(
           name TEXT PRIMARY KEY NOT NULL,
           table_name TEXT NOT NULL,
           condition TEXT NOT NULL,
           action TEXT NOT NULL DEFAULT 'all',
           created_at_ms INTEGER NOT NULL
         ) STRICT, WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS _lilia_masking_rules(
           id TEXT PRIMARY KEY NOT NULL,
           table_name TEXT NOT NULL,
           path TEXT NOT NULL,
           mask_type TEXT NOT NULL,
           exempt_condition TEXT,
           created_at_ms INTEGER NOT NULL
         ) STRICT, WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS _lilia_references(
           name TEXT PRIMARY KEY NOT NULL,
           source_kind TEXT NOT NULL,
           source_name TEXT NOT NULL,
           target_space TEXT NOT NULL,
           key_mapping_json TEXT NOT NULL CHECK(json_valid(key_mapping_json)),
           on_delete TEXT NOT NULL,
           required INTEGER NOT NULL DEFAULT 0,
           created_at_ms INTEGER NOT NULL
         ) STRICT, WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS _lilia_json_indexes(
           space TEXT NOT NULL,
           index_name TEXT NOT NULL,
           json_path TEXT NOT NULL,
           filter_predicate TEXT,
           PRIMARY KEY(space,index_name)
         ) STRICT, WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS _lilia_json_index_entries(
           space TEXT NOT NULL,
           index_name TEXT NOT NULL,
           idx_value TEXT NOT NULL,
           doc_id TEXT NOT NULL,
           PRIMARY KEY(space,index_name,idx_value,doc_id)
         ) STRICT, WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS _lilia_json_idx_doc ON _lilia_json_index_entries(space,doc_id);
         CREATE TABLE IF NOT EXISTS _lilia_ivm_views(
           name TEXT PRIMARY KEY,
           source_table TEXT NOT NULL,
           query TEXT NOT NULL,
           storage_table TEXT NOT NULL,
           group_by TEXT,
           created_at_ms INTEGER NOT NULL
         );
         CREATE VIRTUAL TABLE IF NOT EXISTS _lilia_fts USING fts5(
           collection UNINDEXED,
           doc_id UNINDEXED,
           content,
           tokenize='unicode61'
         );
         CREATE TABLE IF NOT EXISTS _lilia_fts_terms(
           collection TEXT NOT NULL,
           term TEXT NOT NULL,
           doc_count INTEGER NOT NULL DEFAULT 1,
           PRIMARY KEY(collection, term)
         ) STRICT, WITHOUT ROWID;
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
         CREATE TABLE IF NOT EXISTS _lilia_embed_queue(
           space TEXT NOT NULL,
           id TEXT NOT NULL,
           attempts INTEGER NOT NULL DEFAULT 0,
           next_attempt_ms INTEGER NOT NULL DEFAULT 0,
           last_error TEXT,
           PRIMARY KEY(space,id)
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
         CREATE TABLE IF NOT EXISTS _lilia_catalog(
           kind TEXT NOT NULL,
           name TEXT NOT NULL,
           tier TEXT NOT NULL,
           ttl_ms INTEGER,
           eviction_json TEXT NOT NULL CHECK(json_valid(eviction_json)),
           durability TEXT,
           compartment TEXT,
           fle_policy_json TEXT,
           updated_at_ms INTEGER NOT NULL,
           PRIMARY KEY(kind, name)
         ) STRICT, WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS _lilia_compartments(
           id TEXT PRIMARY KEY NOT NULL,
           salt BLOB NOT NULL,
           verify_hash BLOB NOT NULL,
           created_at_ms INTEGER NOT NULL
         ) STRICT, WITHOUT ROWID;
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
         DROP VIEW IF EXISTS main.lilia_kv;
         DROP VIEW IF EXISTS main.lilia_json;
         DROP VIEW IF EXISTS main.lilia_graph_nodes;
         DROP VIEW IF EXISTS main.lilia_graph_edges;
         DROP VIEW IF EXISTS main.lilia_graph_unified;
         DROP VIEW IF EXISTS main.lilia_vectors;
         DROP VIEW IF EXISTS main.lilia_vector_indexes;
         DROP VIEW IF EXISTS main.lilia_timeseries_chunks;
         DROP VIEW IF EXISTS main.lilia_timeseries_policies;
         DROP VIEW IF EXISTS main.lilia_geofences;
         DROP VIEW IF EXISTS main.lilia_gps_points;
         DROP VIEW IF EXISTS main.lilia_fts;
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
         DROP VIEW IF EXISTS main.lilia_json_history;
         CREATE TABLE IF NOT EXISTS _lilia_crdt_counters(
           space TEXT NOT NULL,
           id TEXT NOT NULL,
           actor_id TEXT NOT NULL,
           p_count INTEGER NOT NULL DEFAULT 0,
           n_count INTEGER NOT NULL DEFAULT 0,
           PRIMARY KEY(space, id, actor_id)
         ) STRICT, WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS _lilia_crdt_orsets(
           space TEXT NOT NULL,
           id TEXT NOT NULL,
           element TEXT NOT NULL,
           tag TEXT NOT NULL,
           tombstone_ms INTEGER,
           PRIMARY KEY(space, id, element, tag)
         ) STRICT, WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS _lilia_crdt_orsets_active ON _lilia_crdt_orsets(space, id, tombstone_ms);
         CREATE TABLE IF NOT EXISTS _lilia_crdt_json(
           space TEXT NOT NULL,
           id TEXT NOT NULL,
           state BLOB NOT NULL,
           materialized TEXT NOT NULL CHECK(json_valid(materialized)),
           version INTEGER NOT NULL CHECK(version > 0),
           PRIMARY KEY(space, id)
         ) STRICT, WITHOUT ROWID;
         DROP VIEW IF EXISTS main.lilia_crdt_counters;
         DROP VIEW IF EXISTS main.lilia_crdt_orsets;
         DROP VIEW IF EXISTS main.lilia_crdt_json;
         CREATE TABLE IF NOT EXISTS _lilia_record_hlc(
           model TEXT NOT NULL,
           space TEXT NOT NULL,
           id TEXT NOT NULL,
           hlc TEXT NOT NULL,
           parent_hlc TEXT,
           node_id TEXT NOT NULL,
           updated_at_ms INTEGER NOT NULL,
           PRIMARY KEY(model, space, id)
         ) STRICT, WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS _lilia_conflicts(
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           model TEXT NOT NULL,
           space TEXT NOT NULL,
           doc_id TEXT NOT NULL,
           base_hlc TEXT,
           ours_hlc TEXT NOT NULL,
           theirs_hlc TEXT NOT NULL,
           ours_value TEXT NOT NULL,
           theirs_value TEXT NOT NULL,
           resolved_value TEXT,
           resolution_strategy TEXT NOT NULL,
           status TEXT NOT NULL DEFAULT 'resolved',
           detected_at_ms INTEGER NOT NULL
         );
         DROP VIEW IF EXISTS main.lilia_conflicts;
         DROP VIEW IF EXISTS main.lilia_catalog;
         CREATE TABLE IF NOT EXISTS _lilia_blobs(
           hash TEXT PRIMARY KEY NOT NULL,
           size_bytes INTEGER NOT NULL,
           ref_count INTEGER NOT NULL CHECK(ref_count >= 0),
           created_at_ms INTEGER NOT NULL,
           sealed INTEGER NOT NULL DEFAULT 1
         ) STRICT, WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS _lilia_blob_refs(
           source_kind TEXT NOT NULL,
           source_space TEXT NOT NULL,
           source_id BLOB NOT NULL,
           blob_hash TEXT NOT NULL,
           PRIMARY KEY(source_kind, source_space, source_id, blob_hash)
         ) STRICT, WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS _lilia_blob_refs_hash ON _lilia_blob_refs(blob_hash);
         CREATE INDEX IF NOT EXISTS _lilia_blobs_orphan ON _lilia_blobs(ref_count) WHERE ref_count = 0;
         CREATE TABLE IF NOT EXISTS _lilia_rules(
           name TEXT PRIMARY KEY NOT NULL,
           definition TEXT NOT NULL,
           created_at_ms INTEGER NOT NULL,
           enabled INTEGER NOT NULL DEFAULT 1
         ) STRICT;
         CREATE TABLE IF NOT EXISTS _lilia_cep_window_events(
           rule_name TEXT NOT NULL,
           partition_key TEXT NOT NULL,
           event_timestamp_ms INTEGER NOT NULL,
           event_value REAL,
           event_payload TEXT
         );
         CREATE INDEX IF NOT EXISTS _lilia_idx_cep_window
           ON _lilia_cep_window_events(rule_name, partition_key, event_timestamp_ms);
         CREATE TABLE IF NOT EXISTS _lilia_shards (
           shard_id TEXT PRIMARY KEY NOT NULL,
           period_start_ms INTEGER NOT NULL,
           period_end_ms INTEGER NOT NULL,
           file_name TEXT NOT NULL,
           status TEXT NOT NULL,
           is_compressed INTEGER NOT NULL DEFAULT 0,
           uncompressed_bytes INTEGER NOT NULL DEFAULT 0,
           compressed_bytes INTEGER NOT NULL DEFAULT 0,
           checksum TEXT,
           created_at_ms INTEGER NOT NULL,
           archived_at_ms INTEGER
         ) STRICT, WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS _lilia_shards_status ON _lilia_shards(status);
         CREATE INDEX IF NOT EXISTS _lilia_shards_period ON _lilia_shards(period_start_ms, period_end_ms);
         CREATE TABLE IF NOT EXISTS _lilia_ledger(
           commit_seq INTEGER PRIMARY KEY NOT NULL,
           timestamp_ms INTEGER NOT NULL,
           parent_hash BLOB NOT NULL,
           tx_root BLOB NOT NULL,
           commit_hash BLOB NOT NULL,
           merkle_root BLOB NOT NULL,
           operation_count INTEGER NOT NULL,
           delta_payload BLOB NOT NULL,
           metadata TEXT
         ) STRICT, WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS _lilia_ledger_commit_hash ON _lilia_ledger(commit_hash);
         CREATE INDEX IF NOT EXISTS _lilia_ledger_timestamp ON _lilia_ledger(timestamp_ms);
         CREATE TABLE IF NOT EXISTS _lilia_ledger_leaves(
           model TEXT NOT NULL,
           space TEXT NOT NULL,
           id TEXT NOT NULL,
           version INTEGER NOT NULL,
           commit_seq INTEGER NOT NULL,
           leaf_index INTEGER NOT NULL,
           leaf_hash BLOB NOT NULL,
           PRIMARY KEY(model, space, id, version)
         ) STRICT, WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS _lilia_ledger_leaves_commit ON _lilia_ledger_leaves(commit_seq);
         CREATE INDEX IF NOT EXISTS _lilia_ledger_leaves_hash ON _lilia_ledger_leaves(leaf_hash);
         COMMIT;",
    )?;

    // Safe schema updates for existing tables
    let _ = connection
        .exec("ALTER TABLE _lilia_graph_edges ADD COLUMN weight REAL NOT NULL DEFAULT 1.0;");
    let _ = connection.exec("ALTER TABLE _lilia_graph_nodes ADD COLUMN doc_space TEXT;");
    let _ = connection.exec("ALTER TABLE _lilia_graph_nodes ADD COLUMN doc_id TEXT;");
    let _ = connection.exec("ALTER TABLE _lilia_catalog ADD COLUMN compartment TEXT;");
    let _ = connection.exec("ALTER TABLE _lilia_catalog ADD COLUMN fle_policy_json TEXT;");
    let _ = connection.exec("ALTER TABLE _lilia_catalog ADD COLUMN check_expr TEXT;");
    let _ = connection.exec("ALTER TABLE _lilia_catalog ADD COLUMN auto_embed_json TEXT;");

    // Migration to format_version 2 for _lilia_json
    if let Ok(mut check_cols) = connection.prepare("PRAGMA table_info(_lilia_json);") {
        let mut has_format = false;
        while let Ok(true) = check_cols.step() {
            if check_cols.column_text(1) == "format" {
                has_format = true;
                break;
            }
        }
        drop(check_cols);

        if !has_format {
            let _ = connection.exec(
                "CREATE TABLE IF NOT EXISTS _lilia_json_v2(
                   space TEXT NOT NULL,
                   id TEXT NOT NULL,
                   value ANY NOT NULL,
                   format INTEGER NOT NULL DEFAULT 0,
                   dict_id INTEGER NOT NULL DEFAULT 0,
                   version INTEGER NOT NULL CHECK(version > 0),
                   PRIMARY KEY(space,id)
                 ) STRICT, WITHOUT ROWID;
                 INSERT OR IGNORE INTO _lilia_json_v2(space, id, value, format, dict_id, version)
                   SELECT space, id, value, 0, 0, version FROM _lilia_json;
                 DROP TABLE _lilia_json;
                 ALTER TABLE _lilia_json_v2 RENAME TO _lilia_json;",
            );
        }
        let _ = connection.exec(
            "INSERT INTO _lilia_metadata(key, value) VALUES('format_version', '2')
             ON CONFLICT(key) DO UPDATE SET value = '2';",
        );
    }

    // Migration for _lilia_json_indexes to support partial and expression indexes
    if let Ok(mut check_idx_cols) = connection.prepare("PRAGMA table_info(_lilia_json_indexes);") {
        let mut has_filter = false;
        while let Ok(true) = check_idx_cols.step() {
            if check_idx_cols.column_text(1) == "filter_predicate" {
                has_filter = true;
                break;
            }
        }
        if !has_filter {
            let _ = connection.exec(
                "CREATE TABLE IF NOT EXISTS _lilia_json_indexes_v2(
                   space TEXT NOT NULL,
                   index_name TEXT NOT NULL,
                   json_path TEXT NOT NULL,
                   filter_predicate TEXT,
                   PRIMARY KEY(space,index_name)
                 ) STRICT, WITHOUT ROWID;
                 INSERT OR IGNORE INTO _lilia_json_indexes_v2(space, index_name, json_path, filter_predicate)
                   SELECT space, index_name, json_path, NULL FROM _lilia_json_indexes;
                 DROP TABLE _lilia_json_indexes;
                 ALTER TABLE _lilia_json_indexes_v2 RENAME TO _lilia_json_indexes;
                 CREATE INDEX IF NOT EXISTS _lilia_json_idx_doc ON _lilia_json_index_entries(space,doc_id);",
            );
        }
    }

    connection.exec(
        "CREATE TABLE IF NOT EXISTS _lilia_autotune_query_stats(
           space TEXT NOT NULL,
           fields TEXT NOT NULL,
           executions INTEGER NOT NULL,
           scans INTEGER NOT NULL,
           slow_scans INTEGER NOT NULL,
           scan_rows_examined INTEGER NOT NULL,
           rows_examined INTEGER NOT NULL,
           rows_returned INTEGER NOT NULL,
           total_duration_us INTEGER NOT NULL,
           max_duration_us INTEGER NOT NULL,
           first_seen_ms INTEGER NOT NULL,
           last_seen_ms INTEGER NOT NULL,
           PRIMARY KEY(space,fields)
         ) STRICT, WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS _lilia_autotune_indexes(
           space TEXT NOT NULL,
           index_name TEXT NOT NULL,
           origin TEXT NOT NULL,
           phase TEXT NOT NULL,
           first_seen_ms INTEGER NOT NULL,
           last_used_ms INTEGER,
           lookups INTEGER NOT NULL DEFAULT 0,
           changed_ms INTEGER NOT NULL,
           PRIMARY KEY(space,index_name)
         ) STRICT, WITHOUT ROWID;",
    )?;

    Ok(())
}

pub(crate) fn migrate_ephemeral_sql(connection: &NativeConnection, db_alias: &str) -> Result<()> {
    connection.exec(&format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS {db_alias}._lilia_mem_fts USING fts5(
           collection UNINDEXED,
           doc_id UNINDEXED,
           content,
           tokenize='unicode61'
         );
         CREATE TABLE IF NOT EXISTS {db_alias}._lilia_mem_fts_terms(
           collection TEXT NOT NULL,
           term TEXT NOT NULL,
           doc_count INTEGER NOT NULL DEFAULT 1,
           PRIMARY KEY(collection, term)
         ) STRICT, WITHOUT ROWID;"
    ))
}

/// Creates the sharded data tables inside the attached database `alias`.
///
/// Only the models listed in [`crate::sharding::views::SHARDED_TABLES`] are
/// partitioned; index definitions, revisions, catalog and every other model stay
/// in the coordinator database.
pub(crate) fn migrate_shard_database(connection: &NativeConnection, alias: &str) -> Result<()> {
    connection.exec(&format!(
        "BEGIN IMMEDIATE;
         CREATE TABLE IF NOT EXISTS {alias}._lilia_kv(
           namespace TEXT NOT NULL,
           key BLOB NOT NULL,
           value BLOB NOT NULL,
           version INTEGER NOT NULL CHECK(version > 0),
           expires_at_ms INTEGER,
           PRIMARY KEY(namespace,key)
         ) STRICT, WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS {alias}._lilia_kv_expiry ON _lilia_kv(expires_at_ms)
           WHERE expires_at_ms IS NOT NULL;
         CREATE TABLE IF NOT EXISTS {alias}._lilia_json(
           space TEXT NOT NULL,
           id TEXT NOT NULL,
           value ANY NOT NULL,
           format INTEGER NOT NULL DEFAULT 0,
           dict_id INTEGER NOT NULL DEFAULT 0,
           version INTEGER NOT NULL CHECK(version > 0),
           PRIMARY KEY(space,id)
         ) STRICT, WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS {alias}._lilia_json_index_entries(
           space TEXT NOT NULL,
           index_name TEXT NOT NULL,
           idx_value TEXT NOT NULL,
           doc_id TEXT NOT NULL,
           PRIMARY KEY(space,index_name,idx_value,doc_id)
         ) STRICT, WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS {alias}._lilia_json_idx_doc ON _lilia_json_index_entries(space,doc_id);
         CREATE TABLE IF NOT EXISTS {alias}._lilia_timeseries_chunks(
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
         CREATE INDEX IF NOT EXISTS {alias}._lilia_ts_series_end ON _lilia_timeseries_chunks(series_id,resolution_ms,end_time);
         COMMIT;"
    ))
}

#[allow(clippy::too_many_lines)]
pub(crate) fn init_unified_temp_views(
    connection: &NativeConnection,
    include_vtabs: bool,
    has_mem_db: bool,
) -> Result<()> {
    let kv_sql = if include_vtabs {
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_kv AS
           SELECT namespace, key, lilia_decode_kv(value) AS value, version, expires_at_ms, 'disk' AS tier
           FROM main._lilia_kv k
           WHERE NOT EXISTS (
             SELECT 1 FROM _vtab_buffered_kv o WHERE o.namespace = k.namespace AND o.key = k.key
           )
           UNION ALL
           SELECT namespace, key, value, version, expires_at_ms, 'buffered' AS tier
           FROM _vtab_buffered_kv WHERE deleted = 0
           UNION ALL
           SELECT namespace, key, value, version, expires_at_ms, 'memory' AS tier
           FROM _vtab_mem_kv;"
    } else {
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_kv AS
           SELECT namespace, key, lilia_decode_kv(value) AS value, version, expires_at_ms, 'disk' AS tier
           FROM main._lilia_kv;"
    };

    let json_sql = if include_vtabs {
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_json AS
           SELECT space, id, lilia_decode_json(value) AS value, lilia_decode_json(value) AS data, version, 'disk' AS tier
           FROM main._lilia_json j
           WHERE NOT EXISTS (
             SELECT 1 FROM _vtab_buffered_json o WHERE o.space = j.space AND o.id = j.id
           )
           UNION ALL
           SELECT space, id, value, value AS data, version, 'buffered' AS tier
           FROM _vtab_buffered_json WHERE deleted = 0
           UNION ALL
           SELECT space, id, value, value AS data, version, 'memory' AS tier
           FROM _vtab_mem_json;"
    } else {
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_json AS
           SELECT space, id, lilia_decode_json(value) AS value, lilia_decode_json(value) AS data, version, 'disk' AS tier
           FROM main._lilia_json;"
    };

    let graph_nodes_sql = if include_vtabs {
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_graph_nodes AS
           SELECT graph, id, kind, metadata, doc_space, doc_id, 'disk' AS tier
           FROM main._lilia_graph_nodes
           UNION ALL
           SELECT graph, id, kind, metadata, doc_space, doc_id, 'memory' AS tier
           FROM _vtab_mem_graph_nodes;"
    } else {
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_graph_nodes AS
           SELECT graph, id, kind, metadata, doc_space, doc_id, 'disk' AS tier
           FROM main._lilia_graph_nodes;"
    };

    let graph_edges_sql = if include_vtabs {
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_graph_edges AS
           SELECT graph, source, target, kind, weight, 'disk' AS tier
           FROM main._lilia_graph_edges
           UNION ALL
           SELECT graph, source, target, kind, weight, 'memory' AS tier
           FROM _vtab_mem_graph_edges;"
    } else {
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_graph_edges AS
           SELECT graph, source, target, kind, weight, 'disk' AS tier
           FROM main._lilia_graph_edges;"
    };

    let vectors_sql = if include_vtabs {
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_vectors AS
           SELECT collection, id, dim, element_type, lilia_decode_vector(vector) AS vector, 'disk' AS tier
           FROM main._lilia_vectors
           UNION ALL
           SELECT collection, id, dim, element_type, vector, 'memory' AS tier
           FROM _vtab_mem_vectors;"
    } else {
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_vectors AS
           SELECT collection, id, dim, element_type, lilia_decode_vector(vector) AS vector, 'disk' AS tier
           FROM main._lilia_vectors;"
    };

    let vector_indexes_sql = if include_vtabs {
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_vector_indexes AS
           SELECT collection, entry_id, max_level, m, ef_construction, ef_search, metric, element_type, dim, 'disk' AS tier
           FROM main._lilia_hnsw_meta
           UNION ALL
           SELECT collection, entry_id, max_level, m, ef_construction, ef_search, metric, element_type, dim, 'memory' AS tier
           FROM _vtab_mem_vector_indexes;"
    } else {
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_vector_indexes AS
           SELECT collection, entry_id, max_level, m, ef_construction, ef_search, metric, element_type, dim, 'disk' AS tier
           FROM main._lilia_hnsw_meta;"
    };

    let ts_chunks_sql = if include_vtabs {
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_timeseries_chunks AS
           SELECT series_id, resolution_ms, start_time, end_time, point_count, min_val, max_val, sum_val,
                  (sum_val / point_count) AS avg_val, length(data) AS compressed_bytes, 'disk' AS tier
           FROM main._lilia_timeseries_chunks
           UNION ALL
           SELECT series_id, resolution_ms, start_time, end_time, point_count, min_val, max_val, sum_val,
                  avg_val, compressed_bytes, 'memory' AS tier
           FROM _vtab_mem_ts;"
    } else {
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_timeseries_chunks AS
           SELECT series_id, resolution_ms, start_time, end_time, point_count, min_val, max_val, sum_val,
                  (sum_val / point_count) AS avg_val, length(data) AS compressed_bytes, 'disk' AS tier
           FROM main._lilia_timeseries_chunks;"
    };

    let fts_sql = if has_mem_db {
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_fts AS
           SELECT collection, doc_id, content, 'disk' AS tier FROM main._lilia_fts
           UNION ALL
           SELECT collection, doc_id, content, 'memory' AS tier FROM _lilia_mem._lilia_mem_fts;"
    } else {
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_fts AS
           SELECT collection, doc_id, content, 'disk' AS tier FROM main._lilia_fts;"
    };

    let crdt_counters_sql = if include_vtabs {
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_crdt_counters AS
           SELECT space, id, COALESCE(SUM(p_count) - SUM(n_count), 0) AS value, 'disk' AS tier
           FROM main._lilia_crdt_counters
           GROUP BY space, id
           UNION ALL
           SELECT space, id, value, 'memory' AS tier
           FROM _vtab_mem_crdt_counters;"
    } else {
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_crdt_counters AS
           SELECT space, id, COALESCE(SUM(p_count) - SUM(n_count), 0) AS value, 'disk' AS tier
           FROM main._lilia_crdt_counters
           GROUP BY space, id;"
    };

    let crdt_orsets_sql = if include_vtabs {
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_crdt_orsets AS
           SELECT space, id, element, 'disk' AS tier
           FROM main._lilia_crdt_orsets
           WHERE tombstone_ms IS NULL
           GROUP BY space, id, element
           UNION ALL
           SELECT space, id, element, 'memory' AS tier
           FROM _vtab_mem_crdt_orsets;"
    } else {
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_crdt_orsets AS
           SELECT space, id, element, 'disk' AS tier
           FROM main._lilia_crdt_orsets
           WHERE tombstone_ms IS NULL
           GROUP BY space, id, element;"
    };

    let crdt_json_sql = if include_vtabs {
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_crdt_json AS
           SELECT space, id, materialized AS value, version, 'disk' AS tier
           FROM main._lilia_crdt_json
           UNION ALL
           SELECT space, id, value, version, 'memory' AS tier
           FROM _vtab_mem_crdt_json;"
    } else {
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_crdt_json AS
           SELECT space, id, materialized AS value, version, 'disk' AS tier
           FROM main._lilia_crdt_json;"
    };

    let catalog_sql = if include_vtabs {
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_catalog AS
           SELECT kind, name, tier, ttl_ms, eviction FROM _vtab_catalog;"
    } else {
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_catalog AS
           SELECT kind, name, tier, ttl_ms, eviction_json AS eviction FROM main._lilia_catalog;"
    };

    let static_views = "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_graph_unified AS
           SELECT g.graph, g.id, g.kind, g.metadata,
                  g.doc_space AS doc_space,
                  CASE WHEN g.doc_space IS NULL THEN NULL ELSE COALESCE(g.doc_id, g.id) END AS doc_id,
                  j.value AS document,
                  g.tier AS tier
           FROM temp.lilia_graph_nodes g
           LEFT JOIN temp.lilia_json j
             ON j.space = g.doc_space
            AND j.id = COALESCE(g.doc_id, g.id);
         CREATE TEMP VIEW IF NOT EXISTS temp.lilia_timeseries_policies AS
           SELECT series_pattern, policy_json FROM main._lilia_timeseries_policies;
         CREATE TEMP VIEW IF NOT EXISTS temp.lilia_geofences AS
           SELECT rowid, _shape, id, name, category, metadata FROM main._lilia_geofences;
         CREATE TEMP VIEW IF NOT EXISTS temp.lilia_gps_points AS
           SELECT track_id, timestamp, latitude, longitude, altitude, speed, accuracy FROM main._lilia_gps_points;
         CREATE TEMP VIEW IF NOT EXISTS temp.lilia_json_history AS
           SELECT space, id, version, timestamp_ms, is_keyframe, is_deletion FROM main._lilia_json_revisions;
         CREATE TEMP VIEW IF NOT EXISTS temp.lilia_conflicts AS
           SELECT id, model, space, doc_id, base_hlc, ours_hlc, theirs_hlc, resolution_strategy, status, detected_at_ms
           FROM main._lilia_conflicts;";

    connection.exec(&format!(
        "{kv_sql}\n{json_sql}\n{graph_nodes_sql}\n{graph_edges_sql}\n{vectors_sql}\n{vector_indexes_sql}\n{ts_chunks_sql}\n{fts_sql}\n{crdt_counters_sql}\n{crdt_orsets_sql}\n{crdt_json_sql}\n{catalog_sql}\n{static_views}"
    ))
}

fn take_ascii_word(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    let end = s
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .unwrap_or(s.len());
    if end == 0 {
        None
    } else {
        Some((&s[..end], &s[end..]))
    }
}

#[allow(clippy::too_many_lines)] // one sequential procedure over shared state; splitting it would only thread that state through helpers
pub(crate) fn rewrite_ephemeral_create_sql(sql: &str) -> Result<(String, String)> {
    let trimmed = sql.trim();
    let (kw_create, rest) = take_ascii_word(trimmed).ok_or_else(|| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            "ephemeral DDL must start with CREATE",
            false,
        )
    })?;
    if !kw_create.eq_ignore_ascii_case("CREATE") {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            "ephemeral DDL must start with CREATE",
            false,
        ));
    }
    let (kw_next, mut rest) = take_ascii_word(rest).ok_or_else(|| {
        LiliaError::new(
            ErrorCode::InvalidInput,
            "incomplete CREATE [EPHEMERAL] TABLE statement",
            false,
        )
    })?;
    let kw_table = if kw_next.eq_ignore_ascii_case("EPHEMERAL") {
        let (t, r) = take_ascii_word(rest).ok_or_else(|| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                "expected TABLE after CREATE EPHEMERAL",
                false,
            )
        })?;
        rest = r;
        t
    } else {
        kw_next
    };
    if !kw_table.eq_ignore_ascii_case("TABLE") {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            "ephemeral DDL requires CREATE [EPHEMERAL] TABLE",
            false,
        ));
    }
    let mut if_not_exists = "";
    let after_kw = rest.trim_start();
    if let Some((w1, r1)) = take_ascii_word(after_kw) {
        if w1.eq_ignore_ascii_case("IF") {
            let (w2, r2) = take_ascii_word(r1).ok_or_else(|| {
                LiliaError::new(ErrorCode::InvalidInput, "incomplete IF NOT EXISTS", false)
            })?;
            let (w3, r3) = take_ascii_word(r2).ok_or_else(|| {
                LiliaError::new(ErrorCode::InvalidInput, "incomplete IF NOT EXISTS", false)
            })?;
            if !w2.eq_ignore_ascii_case("NOT") || !w3.eq_ignore_ascii_case("EXISTS") {
                return Err(LiliaError::new(
                    ErrorCode::InvalidInput,
                    "expected IF NOT EXISTS",
                    false,
                ));
            }
            if_not_exists = "IF NOT EXISTS ";
            rest = r3;
        }
    }
    let ident_start = rest.trim_start();
    let (table_name, after_ident) = if let Some(inner) = ident_start.strip_prefix('"') {
        let mut end = None;
        let bytes = inner.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'"' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                    i += 2;
                    continue;
                }
                end = Some(i);
                break;
            }
            i += 1;
        }
        let end = end.ok_or_else(|| {
            LiliaError::new(ErrorCode::InvalidInput, "unclosed quoted identifier", false)
        })?;
        (inner[..end].replace("\"\"", "\""), &inner[end + 1..])
    } else {
        let (w, r) = take_ascii_word(ident_start).ok_or_else(|| {
            LiliaError::new(
                ErrorCode::InvalidInput,
                "missing ephemeral table name",
                false,
            )
        })?;
        (w.to_owned(), r)
    };
    if after_ident.trim_start().starts_with('.') {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            "ephemeral table name cannot include a schema prefix",
            false,
        ));
    }
    validate_name("ephemeral table", &table_name)?;
    if table_name.starts_with("_lilia_") || table_name.starts_with("lilia_") {
        return Err(LiliaError::new(
            ErrorCode::InvalidInput,
            "ephemeral table name cannot use reserved lilia prefix",
            false,
        ));
    }
    let escaped = table_name.replace('"', "\"\"");
    let rewritten = format!("CREATE TABLE {if_not_exists}_lilia_mem.\"{escaped}\"{after_ident}");
    Ok((table_name, rewritten))
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

/// SQLite integer for a count, size or position: saturates at `i64::MAX`.
pub(crate) fn sql_int(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Count, size or position read from a non-negative SQLite integer; a negative value reads as 0.
pub(crate) fn sql_count(value: i64) -> usize {
    usize::try_from(value).unwrap_or_default()
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
