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
///
/// # Errors
/// Propagates errors from the operations it performs.
pub fn refresh_temp_views_on_reader(
    connection: &NativeConnection,
    shard_aliases: &[String],
) -> Result<()> {
    connection.pragma_update("query_only", 0)?;
    let rebuilt = refresh_temp_views(connection, shard_aliases, true, true);
    let restored = connection.pragma_update("query_only", 1);
    rebuilt.and(restored)
}

/// Where the in-memory rows of a unified view come from.
#[derive(Clone, Copy)]
enum MemorySource {
    /// The `_vtab_*` virtual tables over the memory and buffered tiers.
    Vtabs,
    /// The attached `_lilia_mem` database.
    MemDb,
}

/// One unified `temp.lilia_*` view: the union of its disk query and of its memory queries.
struct TierView {
    name: &'static str,
    /// Disk query, where `{schema}` stands for `main` and, when `sharded`, each attached shard.
    disk: &'static str,
    sharded: bool,
    memory_source: MemorySource,
    memory: &'static [&'static str],
}

const TIER_VIEWS: [TierView; 11] = [
    TierView {
        name: "lilia_kv",
        disk: "SELECT namespace, key, lilia_decode_kv(value) AS value, version, expires_at_ms, 'disk' AS tier \
               FROM {schema}._lilia_kv",
        sharded: true,
        memory_source: MemorySource::Vtabs,
        memory: &[
            "SELECT namespace, key, value, version, expires_at_ms, 'buffered' AS tier \
             FROM _vtab_buffered_kv WHERE deleted = 0",
            "SELECT namespace, key, value, version, expires_at_ms, 'memory' AS tier FROM _vtab_mem_kv",
        ],
    },
    TierView {
        name: "lilia_json",
        disk: "SELECT space, id, lilia_decode_json(value) AS value, version, 'disk' AS tier \
               FROM {schema}._lilia_json",
        sharded: true,
        memory_source: MemorySource::Vtabs,
        memory: &[
            "SELECT space, id, value, version, 'buffered' AS tier FROM _vtab_buffered_json WHERE deleted = 0",
            "SELECT space, id, value, version, 'memory' AS tier FROM _vtab_mem_json",
        ],
    },
    TierView {
        name: "lilia_graph_nodes",
        disk: "SELECT graph, id, kind, metadata, doc_space, doc_id, 'disk' AS tier FROM {schema}._lilia_graph_nodes",
        sharded: false,
        memory_source: MemorySource::Vtabs,
        memory: &["SELECT graph, id, kind, metadata, doc_space, doc_id, 'memory' AS tier FROM _vtab_mem_graph_nodes"],
    },
    TierView {
        name: "lilia_graph_edges",
        disk: "SELECT graph, source, target, kind, weight, 'disk' AS tier FROM {schema}._lilia_graph_edges",
        sharded: false,
        memory_source: MemorySource::Vtabs,
        memory: &["SELECT graph, source, target, kind, weight, 'memory' AS tier FROM _vtab_mem_graph_edges"],
    },
    TierView {
        name: "lilia_vectors",
        disk: "SELECT collection, id, dim, element_type, lilia_decode_vector(vector) AS vector, 'disk' AS tier \
               FROM {schema}._lilia_vectors",
        sharded: false,
        memory_source: MemorySource::Vtabs,
        memory: &["SELECT collection, id, dim, element_type, vector, 'memory' AS tier FROM _vtab_mem_vectors"],
    },
    TierView {
        name: "lilia_vector_indexes",
        disk: "SELECT collection, entry_id, max_level, m, ef_construction, ef_search, metric, element_type, dim, \
               'disk' AS tier FROM {schema}._lilia_hnsw_meta",
        sharded: false,
        memory_source: MemorySource::Vtabs,
        memory: &["SELECT collection, entry_id, max_level, m, ef_construction, ef_search, metric, element_type, dim, \
                   'memory' AS tier FROM _vtab_mem_vector_indexes"],
    },
    TierView {
        name: "lilia_timeseries_chunks",
        disk: "SELECT series_id, resolution_ms, start_time, end_time, point_count, min_val, max_val, sum_val, \
               (sum_val / point_count) AS avg_val, length(data) AS compressed_bytes, 'disk' AS tier \
               FROM {schema}._lilia_timeseries_chunks",
        sharded: true,
        memory_source: MemorySource::Vtabs,
        memory: &["SELECT series_id, resolution_ms, start_time, end_time, point_count, min_val, max_val, sum_val, \
                   avg_val, compressed_bytes, 'memory' AS tier FROM _vtab_mem_ts"],
    },
    TierView {
        name: "lilia_fts",
        disk: "SELECT collection, doc_id, content, 'disk' AS tier FROM {schema}._lilia_fts",
        sharded: false,
        memory_source: MemorySource::MemDb,
        memory: &["SELECT collection, doc_id, content, 'memory' AS tier FROM _lilia_mem._lilia_mem_fts"],
    },
    TierView {
        name: "lilia_crdt_counters",
        disk: "SELECT space, id, COALESCE(SUM(p_count) - SUM(n_count), 0) AS value, 'disk' AS tier \
               FROM {schema}._lilia_crdt_counters GROUP BY space, id",
        sharded: false,
        memory_source: MemorySource::Vtabs,
        memory: &["SELECT space, id, value, 'memory' AS tier FROM _vtab_mem_crdt_counters"],
    },
    TierView {
        name: "lilia_crdt_orsets",
        disk: "SELECT space, id, element, 'disk' AS tier FROM {schema}._lilia_crdt_orsets \
               WHERE tombstone_ms IS NULL GROUP BY space, id, element",
        sharded: false,
        memory_source: MemorySource::Vtabs,
        memory: &["SELECT space, id, element, 'memory' AS tier FROM _vtab_mem_crdt_orsets"],
    },
    TierView {
        name: "lilia_crdt_json",
        disk: "SELECT space, id, materialized AS value, version, 'disk' AS tier FROM {schema}._lilia_crdt_json",
        sharded: false,
        memory_source: MemorySource::Vtabs,
        memory: &["SELECT space, id, value, version, 'memory' AS tier FROM _vtab_mem_crdt_json"],
    },
];

/// Rebuilds the unified temporary SQLite views across `main`, in-memory vtabs, and all currently attached shards.
///
/// # Errors
/// Propagates errors from the operations it performs.
pub fn refresh_temp_views(
    connection: &NativeConnection,
    shard_aliases: &[String],
    include_vtabs: bool,
    has_mem_db: bool,
) -> Result<()> {
    for view in &TIER_VIEWS {
        connection.exec(&format!("DROP VIEW IF EXISTS temp.{};", view.name))?;
    }
    for view in &TIER_VIEWS {
        let shards = if view.sharded { shard_aliases } else { &[] };
        let mut parts: Vec<String> = std::iter::once("main")
            .chain(shards.iter().map(String::as_str))
            .map(|schema| view.disk.replace("{schema}", schema))
            .collect();
        let with_memory = match view.memory_source {
            MemorySource::Vtabs => include_vtabs,
            MemorySource::MemDb => has_mem_db,
        };
        if with_memory {
            parts.extend(view.memory.iter().map(|query| (*query).to_owned()));
        }
        connection.exec(&format!(
            "CREATE TEMP VIEW IF NOT EXISTS temp.{} AS\n{};",
            view.name,
            parts.join("\nUNION ALL\n")
        ))?;
    }
    refresh_shadow_views(connection, shard_aliases)
}
