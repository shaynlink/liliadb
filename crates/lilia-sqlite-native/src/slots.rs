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
    Count = 25,
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
    ];

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
                "INSERT INTO _lilia_json(space, id, value, version) \
                 VALUES(?1, ?2, ?3, 1) \
                 ON CONFLICT(space, id) DO UPDATE SET \
                   value = excluded.value, \
                   version = _lilia_json.version + 1 \
                 RETURNING version;"
            }
            Self::JsonPutVersioned => {
                "INSERT INTO _lilia_json(space, id, value, version) \
                 VALUES(?1, ?2, ?3, ?4) \
                 ON CONFLICT(space, id) DO UPDATE SET \
                   value = excluded.value, \
                   version = excluded.version;"
            }
            Self::JsonGet => {
                "SELECT value, version FROM _lilia_json WHERE space = ?1 AND id = ?2;"
            }
            Self::JsonScan => {
                "SELECT id, value, version FROM _lilia_json WHERE space = ?1 AND id > ?2 ORDER BY id LIMIT ?3;"
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
}
