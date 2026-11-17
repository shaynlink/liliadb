use crate::statement::NativeStatement;

#[repr(usize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StmtSlot {
    KvUpsertReturning = 0,
    KvUpsertVersioned = 1,
    KvGet = 2,
    KvScan = 3,
    KvDelete = 4,
    KvVersion = 5,
    JsonPutReturning = 6,
    JsonPutVersioned = 7,
    JsonGet = 8,
    JsonScan = 9,
    JsonDelete = 10,
    JsonVersion = 11,
    GraphNodeSet = 12,
    GraphNodeDelete = 13,
    GraphEdgeSet = 14,
    GraphEdgeDelete = 15,
    FtsIndexDelete = 16,
    FtsIndexInsert = 17,
    VectorUpsert = 18,
    VectorDelete = 19,
    VectorGet = 20,
    HnswMetaUpsert = 21,
    HnswMetaGet = 22,
    HnswEdgesUpsert = 23,
    HnswEdgesDelete = 24,
    MemFtsIndexDelete = 25,
    MemFtsIndexInsert = 26,
    KvLocate = 27,
    JsonLocate = 28,
    EmbedEnqueue = 29,
    EmbedDequeue = 30,
    Count = 31,
}

impl StmtSlot {
    pub const ALL: [StmtSlot; Self::Count as usize] = [
        StmtSlot::KvUpsertReturning,
        StmtSlot::KvUpsertVersioned,
        StmtSlot::KvGet,
        StmtSlot::KvScan,
        StmtSlot::KvDelete,
        StmtSlot::KvVersion,
        StmtSlot::JsonPutReturning,
        StmtSlot::JsonPutVersioned,
        StmtSlot::JsonGet,
        StmtSlot::JsonScan,
        StmtSlot::JsonDelete,
        StmtSlot::JsonVersion,
        StmtSlot::GraphNodeSet,
        StmtSlot::GraphNodeDelete,
        StmtSlot::GraphEdgeSet,
        StmtSlot::GraphEdgeDelete,
        StmtSlot::FtsIndexDelete,
        StmtSlot::FtsIndexInsert,
        StmtSlot::VectorUpsert,
        StmtSlot::VectorDelete,
        StmtSlot::VectorGet,
        StmtSlot::HnswMetaUpsert,
        StmtSlot::HnswMetaGet,
        StmtSlot::HnswEdgesUpsert,
        StmtSlot::HnswEdgesDelete,
        StmtSlot::MemFtsIndexDelete,
        StmtSlot::MemFtsIndexInsert,
        StmtSlot::KvLocate,
        StmtSlot::JsonLocate,
        StmtSlot::EmbedEnqueue,
        StmtSlot::EmbedDequeue,
    ];

    /// Select FTS delete slot for disk (`main`) or memory (`_lilia_mem`).
    #[must_use]
    pub const fn fts_delete_for(is_memory: bool) -> Self {
        if is_memory {
            Self::MemFtsIndexDelete
        } else {
            Self::FtsIndexDelete
        }
    }

    /// Select FTS insert slot for disk (`main`) or memory (`_lilia_mem`).
    #[must_use]
    pub const fn fts_insert_for(is_memory: bool) -> Self {
        if is_memory {
            Self::MemFtsIndexInsert
        } else {
            Self::FtsIndexInsert
        }
    }

    /// The sharded data table this slot reads or writes, when it targets one.
    #[must_use]
    pub const fn sharded_table(&self) -> Option<&'static str> {
        match self {
            Self::KvUpsertReturning
            | Self::KvUpsertVersioned
            | Self::KvGet
            | Self::KvScan
            | Self::KvDelete
            | Self::KvVersion => Some("_lilia_kv"),
            Self::JsonPutReturning
            | Self::JsonPutVersioned
            | Self::JsonGet
            | Self::JsonScan
            | Self::JsonDelete
            | Self::JsonVersion => Some("_lilia_json"),
            _ => None,
        }
    }

    /// The slot SQL with its sharded table qualified by `schema` (an attached
    /// database alias). Slots without a sharded table are returned unchanged.
    #[must_use]
    pub fn sql_in_schema(&self, schema: &str) -> String {
        let sql = self.sql();
        match self.sharded_table() {
            Some(table) => sql
                .replace(
                    &format!("INTO {table}("),
                    &format!("INTO {schema}.{table}("),
                )
                .replace(
                    &format!("FROM {table} "),
                    &format!("FROM {schema}.{table} "),
                ),
            None => sql.to_owned(),
        }
    }

    pub fn sql(&self) -> &'static str {
        match self {
            Self::KvUpsertReturning => {
                "INSERT INTO _lilia_kv(namespace, key, value, version, expires_at_ms) \
                 VALUES(?1, ?2, ?3, 1, ?4) \
                 ON CONFLICT(namespace, key) DO UPDATE SET \
                   value = excluded.value, \
                   version = _lilia_kv.version + 1, \
                   expires_at_ms = excluded.expires_at_ms \
                 RETURNING version;"
            }
            Self::KvUpsertVersioned => {
                "INSERT INTO _lilia_kv(namespace, key, value, version, expires_at_ms) \
                 VALUES(?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(namespace, key) DO UPDATE SET \
                   value = excluded.value, \
                   version = excluded.version, \
                   expires_at_ms = excluded.expires_at_ms;"
            }
            Self::KvGet => {
                "SELECT value, version, expires_at_ms FROM _lilia_kv \
                 WHERE namespace = ?1 AND key = ?2 AND (expires_at_ms IS NULL OR expires_at_ms > ?3);"
            }
            Self::KvScan => {
                "SELECT key, value, version, expires_at_ms FROM _lilia_kv \
                 WHERE namespace = ?1 AND key > ?2 AND (expires_at_ms IS NULL OR expires_at_ms > ?3) \
                 ORDER BY key LIMIT ?4;"
            }
            Self::KvDelete => {
                "DELETE FROM _lilia_kv WHERE namespace = ?1 AND key = ?2;"
            }
            Self::KvVersion => {
                "SELECT version FROM _lilia_kv WHERE namespace = ?1 AND key = ?2;"
            }
            Self::JsonPutReturning => {
                "INSERT INTO _lilia_json(space, id, value, format, dict_id, version) \
                 VALUES(?1, ?2, ?3, ?4, ?5, 1) \
                 ON CONFLICT(space, id) DO UPDATE SET \
                   value = excluded.value, \
                   format = excluded.format, \
                   dict_id = excluded.dict_id, \
                   version = _lilia_json.version + 1 \
                 RETURNING version;"
            }
            Self::JsonPutVersioned => {
                "INSERT INTO _lilia_json(space, id, value, format, dict_id, version) \
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT(space, id) DO UPDATE SET \
                   value = excluded.value, \
                   format = excluded.format, \
                   dict_id = excluded.dict_id, \
                   version = excluded.version;"
            }
            Self::JsonGet => {
                "SELECT value, format, dict_id, version FROM _lilia_json WHERE space = ?1 AND id = ?2;"
            }
            Self::JsonScan => {
                "SELECT id, value, format, dict_id, version FROM _lilia_json WHERE space = ?1 AND id > ?2 ORDER BY id LIMIT ?3;"
            }
            Self::JsonDelete => {
                "DELETE FROM _lilia_json WHERE space = ?1 AND id = ?2;"
            }
            Self::JsonVersion => {
                "SELECT version FROM _lilia_json WHERE space = ?1 AND id = ?2;"
            }
            Self::GraphNodeSet => {
                "INSERT INTO _lilia_graph_nodes(graph, id, kind, metadata, doc_space, doc_id) VALUES(?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT(graph, id) DO UPDATE SET kind = excluded.kind, metadata = excluded.metadata, doc_space = excluded.doc_space, doc_id = excluded.doc_id;"
            }
            Self::GraphNodeDelete => {
                "DELETE FROM _lilia_graph_nodes WHERE graph = ?1 AND id = ?2;"
            }
            Self::GraphEdgeSet => {
                "INSERT INTO _lilia_graph_edges(graph, source, target, kind, weight) VALUES(?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(graph, source, target, kind) DO UPDATE SET weight = excluded.weight;"
            }
            Self::GraphEdgeDelete => {
                "DELETE FROM _lilia_graph_edges WHERE graph = ?1 AND source = ?2 AND target = ?3 AND kind = ?4;"
            }
            Self::FtsIndexDelete => {
                "DELETE FROM _lilia_fts WHERE collection = ?1 AND doc_id = ?2;"
            }
            Self::FtsIndexInsert => {
                "INSERT INTO _lilia_fts(collection, doc_id, content) VALUES(?1, ?2, ?3);"
            }
            Self::VectorUpsert => {
                "INSERT INTO _lilia_vectors(collection, id, dim, element_type, vector) \
                 VALUES(?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(collection, id) DO UPDATE SET \
                   dim = excluded.dim, \
                   element_type = excluded.element_type, \
                   vector = excluded.vector;"
            }
            Self::VectorDelete => {
                "DELETE FROM _lilia_vectors WHERE collection = ?1 AND id = ?2;"
            }
            Self::VectorGet => {
                "SELECT dim, element_type, vector FROM _lilia_vectors WHERE collection = ?1 AND id = ?2;"
            }
            Self::HnswMetaUpsert => {
                "INSERT INTO _lilia_hnsw_meta(collection, entry_id, max_level, m, ef_construction, ef_search, metric, element_type, dim) \
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
                 ON CONFLICT(collection) DO UPDATE SET \
                   entry_id = excluded.entry_id, \
                   max_level = excluded.max_level, \
                   ef_search = excluded.ef_search;"
            }
            Self::HnswMetaGet => {
                "SELECT entry_id, max_level, m, ef_construction, ef_search, metric, element_type, dim \
                 FROM _lilia_hnsw_meta WHERE collection = ?1;"
            }
            Self::HnswEdgesUpsert => {
                "INSERT INTO _lilia_hnsw_edges(collection, id, level, neighbors) \
                 VALUES(?1, ?2, ?3, ?4) \
                 ON CONFLICT(collection, id, level) DO UPDATE SET \
                   neighbors = excluded.neighbors;"
            }
            Self::HnswEdgesDelete => {
                "DELETE FROM _lilia_hnsw_edges WHERE collection = ?1 AND id = ?2;"
            }
            Self::MemFtsIndexDelete => {
                "DELETE FROM _lilia_mem._lilia_mem_fts WHERE collection = ?1 AND doc_id = ?2;"
            }
            Self::MemFtsIndexInsert => {
                "INSERT INTO _lilia_mem._lilia_mem_fts(collection, doc_id, content) VALUES(?1, ?2, ?3);"
            }
            Self::KvLocate => {
                "SELECT _shard FROM _lilia_kv WHERE namespace = ?1 AND key = ?2 LIMIT 1;"
            }
            Self::JsonLocate => {
                "SELECT _shard FROM _lilia_json WHERE space = ?1 AND id = ?2 LIMIT 1;"
            }
            Self::EmbedEnqueue => {
                "INSERT INTO _lilia_embed_queue(space, id) VALUES(?1, ?2) \
                 ON CONFLICT(space, id) DO UPDATE SET \
                   attempts = 0, \
                   next_attempt_ms = 0, \
                   last_error = NULL;"
            }
            Self::EmbedDequeue => {
                "DELETE FROM _lilia_embed_queue WHERE space = ?1 AND id = ?2;"
            }
            Self::Count => unreachable!(),
        }
    }
}

pub struct StatementSlots {
    slots: [Option<NativeStatement>; StmtSlot::Count as usize],
}

impl StatementSlots {
    pub fn empty() -> Self {
        const INIT: Option<NativeStatement> = None;
        Self {
            slots: [INIT; StmtSlot::Count as usize],
        }
    }

    #[inline]
    pub fn get_mut(&mut self, slot: StmtSlot) -> Option<&mut NativeStatement> {
        self.slots[slot as usize].as_mut()
    }

    #[inline]
    pub fn set(&mut self, slot: StmtSlot, stmt: NativeStatement) {
        self.slots[slot as usize] = Some(stmt);
    }

    /// Resets every prepared statement, ending the reads they keep open.
    pub fn reset_all(&mut self) {
        for stmt in self.slots.iter_mut().flatten() {
            // `sqlite3_reset` only repeats the error of the last step, already reported to its caller.
            let _ = stmt.reset();
        }
    }
}
