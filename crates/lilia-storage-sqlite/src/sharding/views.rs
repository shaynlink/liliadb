use lilia_core::Result;
use lilia_sqlite_native::NativeConnection;

/// Data tables that are physically partitioned across shards. Only these models
/// live in shard files; every other table stays in the coordinator database.
pub const SHARDED_TABLES: [&str; 4] = [
    "_lilia_kv",
    "_lilia_json",
    "_lilia_json_index_entries",
    "_lilia_timeseries_chunks",
];

/// Shadows each sharded table with a temp view over every attached shard plus
/// the coordinator's own table (`main`, which keeps data written before
/// sharding was enabled).
///
/// SQLite resolves unqualified names in the temp schema first, so existing
/// read statements transparently span all shards. The extra trailing `_shard`
/// column names the schema a row lives in, which write routing relies on.
/// Writes never go through these views: they are qualified with an explicit
/// schema by the connection's write schema.
fn refresh_shadow_views(connection: &NativeConnection, shard_aliases: &[String]) -> Result<()> {
    for table in SHARDED_TABLES {
        let mut parts: Vec<String> = shard_aliases
            .iter()
            .rev()
            .map(|alias| format!("SELECT *, '{alias}' AS _shard FROM {alias}.{table}"))
            .collect();
        parts.push(format!("SELECT *, 'main' AS _shard FROM main.{table}"));
        connection.exec(&format!(
            "DROP VIEW IF EXISTS temp.{table};\nCREATE TEMP VIEW temp.{table} AS\n{};",
            parts.join("\nUNION ALL\n")
        ))?;
    }
    Ok(())
}

/// Rebuilds the unified temporary views on a pooled reader connection.
///
/// Readers run with `query_only` enabled, which also blocks writes to the
/// per-connection temp schema, so the pragma is lifted for the duration of the
/// rebuild and always restored.
pub fn refresh_temp_views_on_reader(
    connection: &NativeConnection,
    shard_aliases: &[String],
) -> Result<()> {
    connection.pragma_update("query_only", 0)?;
    let rebuilt = refresh_temp_views(connection, shard_aliases, true, true);
    let restored = connection.pragma_update("query_only", 1);
    rebuilt.and(restored)
}

/// Rebuilds the unified temporary SQLite views across `main`, in-memory vtabs, and all currently attached shards.
#[allow(clippy::too_many_lines)]
pub fn refresh_temp_views(
    connection: &NativeConnection,
    shard_aliases: &[String],
    include_vtabs: bool,
    has_mem_db: bool,
) -> Result<()> {
    // 1. Drop existing temporary views
    let drop_views_sql = "
        DROP VIEW IF EXISTS temp.lilia_kv;
        DROP VIEW IF EXISTS temp.lilia_json;
        DROP VIEW IF EXISTS temp.lilia_graph_nodes;
        DROP VIEW IF EXISTS temp.lilia_graph_edges;
        DROP VIEW IF EXISTS temp.lilia_vectors;
        DROP VIEW IF EXISTS temp.lilia_vector_indexes;
        DROP VIEW IF EXISTS temp.lilia_timeseries_chunks;
        DROP VIEW IF EXISTS temp.lilia_fts;
        DROP VIEW IF EXISTS temp.lilia_crdt_counters;
        DROP VIEW IF EXISTS temp.lilia_crdt_orsets;
        DROP VIEW IF EXISTS temp.lilia_crdt_json;
    ";
    connection.exec(drop_views_sql)?;

    // 2. Build KV view
    let mut kv_parts = Vec::new();
    kv_parts.push(
        "SELECT namespace, key, lilia_decode_kv(value) AS value, version, expires_at_ms, 'disk' AS tier \
         FROM main._lilia_kv k".to_string()
    );
    for alias in shard_aliases {
        kv_parts.push(format!(
            "SELECT namespace, key, lilia_decode_kv(value) AS value, version, expires_at_ms, 'disk' AS tier \
             FROM {alias}._lilia_kv"
        ));
    }
    if include_vtabs {
        kv_parts.push(
            "SELECT namespace, key, value, version, expires_at_ms, 'buffered' AS tier \
             FROM _vtab_buffered_kv WHERE deleted = 0".to_string()
        );
        kv_parts.push(
            "SELECT namespace, key, value, version, expires_at_ms, 'memory' AS tier \
             FROM _vtab_mem_kv".to_string()
        );
    }
    let kv_sql = format!(
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_kv AS\n{};",
        kv_parts.join("\nUNION ALL\n")
    );
    connection.exec(&kv_sql)?;

    // 3. Build JSON view
    let mut json_parts = Vec::new();
    json_parts.push(
        "SELECT space, id, lilia_decode_json(value) AS value, version, 'disk' AS tier \
         FROM main._lilia_json j".to_string()
    );
    for alias in shard_aliases {
        json_parts.push(format!(
            "SELECT space, id, lilia_decode_json(value) AS value, version, 'disk' AS tier \
             FROM {alias}._lilia_json"
        ));
    }
    if include_vtabs {
        json_parts.push(
            "SELECT space, id, value, version, 'buffered' AS tier \
             FROM _vtab_buffered_json WHERE deleted = 0".to_string()
        );
        json_parts.push(
            "SELECT space, id, value, version, 'memory' AS tier \
             FROM _vtab_mem_json".to_string()
        );
    }
    let json_sql = format!(
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_json AS\n{};",
        json_parts.join("\nUNION ALL\n")
    );
    connection.exec(&json_sql)?;

    // 4. Build Graph Nodes view
    let mut gn_parts = Vec::new();
    gn_parts.push(
        "SELECT graph, id, kind, metadata, doc_space, doc_id, 'disk' AS tier \
         FROM main._lilia_graph_nodes".to_string()
    );
    if include_vtabs {
        gn_parts.push(
            "SELECT graph, id, kind, metadata, doc_space, doc_id, 'memory' AS tier \
             FROM _vtab_mem_graph_nodes".to_string()
        );
    }
    let gn_sql = format!(
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_graph_nodes AS\n{};",
        gn_parts.join("\nUNION ALL\n")
    );
    connection.exec(&gn_sql)?;

    // 5. Build Graph Edges view
    let mut ge_parts = Vec::new();
    ge_parts.push(
        "SELECT graph, source, target, kind, weight, 'disk' AS tier \
         FROM main._lilia_graph_edges".to_string()
    );
    if include_vtabs {
        ge_parts.push(
            "SELECT graph, source, target, kind, weight, 'memory' AS tier \
             FROM _vtab_mem_graph_edges".to_string()
        );
    }
    let ge_sql = format!(
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_graph_edges AS\n{};",
        ge_parts.join("\nUNION ALL\n")
    );
    connection.exec(&ge_sql)?;

    // 6. Build Vectors view
    let mut vec_parts = Vec::new();
    vec_parts.push(
        "SELECT collection, id, dim, element_type, lilia_decode_vector(vector) AS vector, 'disk' AS tier \
         FROM main._lilia_vectors".to_string()
    );
    if include_vtabs {
        vec_parts.push(
            "SELECT collection, id, dim, element_type, vector, 'memory' AS tier \
             FROM _vtab_mem_vectors".to_string()
        );
    }
    let vec_sql = format!(
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_vectors AS\n{};",
        vec_parts.join("\nUNION ALL\n")
    );
    connection.exec(&vec_sql)?;

    // 7. Build Vector Indexes view
    let mut vi_parts = Vec::new();
    vi_parts.push(
        "SELECT collection, entry_id, max_level, m, ef_construction, ef_search, metric, element_type, dim, 'disk' AS tier \
         FROM main._lilia_hnsw_meta".to_string()
    );
    if include_vtabs {
        vi_parts.push(
            "SELECT collection, entry_id, max_level, m, ef_construction, ef_search, metric, element_type, dim, 'memory' AS tier \
             FROM _vtab_mem_vector_indexes".to_string()
        );
    }
    let vi_sql = format!(
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_vector_indexes AS\n{};",
        vi_parts.join("\nUNION ALL\n")
    );
    connection.exec(&vi_sql)?;

    // 8. Build TimeSeries Chunks view
    let mut ts_parts = Vec::new();
    ts_parts.push(
        "SELECT series_id, resolution_ms, start_time, end_time, point_count, min_val, max_val, sum_val, \
         (sum_val / point_count) AS avg_val, length(data) AS compressed_bytes, 'disk' AS tier \
         FROM main._lilia_timeseries_chunks".to_string()
    );
    for alias in shard_aliases {
        ts_parts.push(format!(
            "SELECT series_id, resolution_ms, start_time, end_time, point_count, min_val, max_val, sum_val, \
             (sum_val / point_count) AS avg_val, length(data) AS compressed_bytes, 'disk' AS tier \
             FROM {alias}._lilia_timeseries_chunks"
        ));
    }
    if include_vtabs {
        ts_parts.push(
            "SELECT series_id, resolution_ms, start_time, end_time, point_count, min_val, max_val, sum_val, \
             avg_val, compressed_bytes, 'memory' AS tier \
             FROM _vtab_mem_ts".to_string()
        );
    }
    let ts_sql = format!(
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_timeseries_chunks AS\n{};",
        ts_parts.join("\nUNION ALL\n")
    );
    connection.exec(&ts_sql)?;

    // 9. Build FTS view
    let mut fts_parts = Vec::new();
    fts_parts.push(
        "SELECT collection, doc_id, content, 'disk' AS tier FROM main._lilia_fts".to_string()
    );
    if has_mem_db {
        fts_parts.push(
            "SELECT collection, doc_id, content, 'memory' AS tier FROM _lilia_mem._lilia_mem_fts".to_string()
        );
    }
    let fts_sql = format!(
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_fts AS\n{};",
        fts_parts.join("\nUNION ALL\n")
    );
    connection.exec(&fts_sql)?;

    // 10. Build CRDT Counters view
    let mut crdt_c_parts = Vec::new();
    crdt_c_parts.push(
        "SELECT space, id, COALESCE(SUM(p_count) - SUM(n_count), 0) AS value, 'disk' AS tier \
         FROM main._lilia_crdt_counters GROUP BY space, id".to_string()
    );
    if include_vtabs {
        crdt_c_parts.push(
            "SELECT space, id, value, 'memory' AS tier FROM _vtab_mem_crdt_counters".to_string()
        );
    }
    let crdt_c_sql = format!(
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_crdt_counters AS\n{};",
        crdt_c_parts.join("\nUNION ALL\n")
    );
    connection.exec(&crdt_c_sql)?;

    // 11. Build CRDT ORSet view
    let mut crdt_o_parts = Vec::new();
    crdt_o_parts.push(
        "SELECT space, id, element, 'disk' AS tier FROM main._lilia_crdt_orsets \
         WHERE tombstone_ms IS NULL GROUP BY space, id, element".to_string()
    );
    if include_vtabs {
        crdt_o_parts.push(
            "SELECT space, id, element, 'memory' AS tier FROM _vtab_mem_crdt_orsets".to_string()
        );
    }
    let crdt_o_sql = format!(
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_crdt_orsets AS\n{};",
        crdt_o_parts.join("\nUNION ALL\n")
    );
    connection.exec(&crdt_o_sql)?;

    // 12. Build CRDT JSON view
    let mut crdt_j_parts = Vec::new();
    crdt_j_parts.push(
        "SELECT space, id, materialized AS value, version, 'disk' AS tier FROM main._lilia_crdt_json".to_string()
    );
    if include_vtabs {
        crdt_j_parts.push(
            "SELECT space, id, value, version, 'memory' AS tier FROM _vtab_mem_crdt_json".to_string()
        );
    }
    let crdt_j_sql = format!(
        "CREATE TEMP VIEW IF NOT EXISTS temp.lilia_crdt_json AS\n{};",
        crdt_j_parts.join("\nUNION ALL\n")
    );
    connection.exec(&crdt_j_sql)?;

    refresh_shadow_views(connection, shard_aliases)?;
    Ok(())
}
