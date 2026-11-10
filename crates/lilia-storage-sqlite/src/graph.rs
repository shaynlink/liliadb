#![allow(clippy::missing_errors_doc)]

use lilia_sqlite_native::StmtSlot;

use crate::storage::{normalize_limit, validate_name};
use crate::Database;
use lilia_core::{Direction, ErrorCode, GraphEdge, GraphNode, LiliaError, Result};

impl Database {
    pub fn graph_set_node(
        &self,
        graph: &str,
        id: &str,
        kind: &str,
        metadata: &serde_json::Value,
    ) -> Result<()> {
        self.graph_set_node_doc(graph, id, kind, None, None, metadata)
    }

    pub fn graph_set_node_doc(
        &self,
        graph: &str,
        id: &str,
        kind: &str,
        doc_space: Option<&str>,
        doc_id: Option<&str>,
        metadata: &serde_json::Value,
    ) -> Result<()> {
        validate_name("graph", graph)?;
        validate_name("id", id)?;
        let meta_str = serde_json::to_string(metadata)
            .map_err(|e| LiliaError::new(ErrorCode::InvalidInput, e.to_string(), false))?;
        let mut writer = self.lock_writer()?;
        let stmt = writer.slot(StmtSlot::GraphNodeSet)?;
        stmt.reset()?;
        stmt.bind_text_static(1, graph)?;
        stmt.bind_text_static(2, id)?;
        stmt.bind_text_static(3, kind)?;
        stmt.bind_text_static(4, &meta_str)?;
        if let Some(space) = doc_space {
            stmt.bind_text_static(5, space)?;
        } else {
            stmt.bind_null(5)?;
        }
        if let Some(did) = doc_id {
            stmt.bind_text_static(6, did)?;
        } else {
            stmt.bind_null(6)?;
        }
        stmt.step()?;
        stmt.reset()?;
        Ok(())
    }

    pub fn graph_get_node(&self, graph: &str, id: &str) -> Result<Option<GraphNode>> {
        validate_name("graph", graph)?;
        validate_name("id", id)?;
        let conn = self.read_lock()?;
        let mut stmt = conn.prepare(
            "SELECT kind, metadata, doc_space, doc_id FROM _lilia_graph_nodes WHERE graph = ?1 AND id = ?2;",
        )?;
        stmt.bind_text_static(1, graph)?;
        stmt.bind_text_static(2, id)?;
        if stmt.step()? {
            let kind = stmt.column_text(0).to_string();
            let meta_str = stmt.column_text(1);
            let metadata = serde_json::from_str(meta_str)
                .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
            let doc_space = if stmt.column_is_null(2) {
                None
            } else {
                Some(stmt.column_text(2).to_string())
            };
            let doc_id = if stmt.column_is_null(3) {
                None
            } else {
                Some(stmt.column_text(3).to_string())
            };
            Ok(Some(GraphNode {
                graph: graph.to_owned(),
                id: id.to_owned(),
                kind,
                metadata,
                doc_space,
                doc_id,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn graph_get_node_document(
        &self,
        graph: &str,
        id: &str,
    ) -> Result<Option<serde_json::Value>> {
        let Some(node) = self.graph_get_node(graph, id)? else {
            return Ok(None);
        };
        if let Some(ref space) = node.doc_space {
            let doc_id = node.doc_id.as_deref().unwrap_or(id);
            return Ok(self.json_get(space, doc_id)?.map(|e| e.value));
        }
        // Cascade resolution: check if kind matches an existing JSON space containing id
        if let Ok(Some(entry)) = self.json_get(&node.kind, id) {
            return Ok(Some(entry.value));
        }
        Ok(None)
    }

    pub fn graph_delete_node(&self, graph: &str, id: &str) -> Result<bool> {
        validate_name("graph", graph)?;
        validate_name("id", id)?;
        let mut writer = self.lock_writer()?;
        let mut tx = writer.transaction()?;
        let mut stmt_edges = tx.prepare(
            "DELETE FROM _lilia_graph_edges WHERE graph = ?1 AND (source = ?2 OR target = ?2);",
        )?;
        stmt_edges.bind_text_static(1, graph)?;
        stmt_edges.bind_text_static(2, id)?;
        stmt_edges.step()?;
        drop(stmt_edges);

        let stmt_node = tx.slot(StmtSlot::GraphNodeDelete)?;
        stmt_node.reset()?;
        stmt_node.bind_text_static(1, graph)?;
        stmt_node.bind_text_static(2, id)?;
        stmt_node.step()?;
        stmt_node.reset()?;
        let deleted = tx.conn().changes() > 0;
        tx.commit()?;
        Ok(deleted)
    }

    pub fn graph_list_nodes(
        &self,
        graph: &str,
        kind_filter: Option<&str>,
        limit: u32,
    ) -> Result<Vec<GraphNode>> {
        validate_name("graph", graph)?;
        let conn = self.read_lock()?;
        let mut stmt = if kind_filter.is_some() {
            conn.prepare(
                "SELECT id, kind, metadata, doc_space, doc_id FROM _lilia_graph_nodes WHERE graph = ?1 AND kind = ?2 ORDER BY id LIMIT ?3;",
            )?
        } else {
            conn.prepare(
                "SELECT id, kind, metadata, doc_space, doc_id FROM _lilia_graph_nodes WHERE graph = ?1 ORDER BY id LIMIT ?2;",
            )?
        };

        stmt.bind_text_static(1, graph)?;
        if let Some(kind) = kind_filter {
            stmt.bind_text_static(2, kind)?;
            stmt.bind_int64(3, normalize_limit(limit))?;
        } else {
            stmt.bind_int64(2, normalize_limit(limit))?;
        }

        let mut nodes = Vec::new();
        while stmt.step()? {
            let id = stmt.column_text(0).to_string();
            let kind = stmt.column_text(1).to_string();
            let meta_str = stmt.column_text(2);
            let metadata = serde_json::from_str(meta_str)
                .map_err(|e| LiliaError::new(ErrorCode::Storage, e.to_string(), false))?;
            let doc_space = if stmt.column_is_null(3) {
                None
            } else {
                Some(stmt.column_text(3).to_string())
            };
            let doc_id = if stmt.column_is_null(4) {
                None
            } else {
                Some(stmt.column_text(4).to_string())
            };
            nodes.push(GraphNode {
                graph: graph.to_owned(),
                id,
                kind,
                metadata,
                doc_space,
                doc_id,
            });
        }
        Ok(nodes)
    }

    pub fn graph_add_edge(
        &self,
        graph: &str,
        source: &str,
        target: &str,
        kind: &str,
    ) -> Result<()> {
        self.graph_add_edge_weighted(graph, source, target, kind, 1.0)
    }

    pub fn graph_add_edge_weighted(
        &self,
        graph: &str,
        source: &str,
        target: &str,
        kind: &str,
        weight: f64,
    ) -> Result<()> {
        validate_name("graph", graph)?;
        validate_name("source", source)?;
        validate_name("target", target)?;
        let mut writer = self.lock_writer()?;
        let stmt = writer.slot(StmtSlot::GraphEdgeSet)?;
        stmt.reset()?;
        stmt.bind_text_static(1, graph)?;
        stmt.bind_text_static(2, source)?;
        stmt.bind_text_static(3, target)?;
        stmt.bind_text_static(4, kind)?;
        stmt.bind_double(5, weight)?;
        stmt.step()?;
        stmt.reset()?;
        Ok(())
    }

    pub fn graph_remove_edge(
        &self,
        graph: &str,
        source: &str,
        target: &str,
        kind: &str,
    ) -> Result<bool> {
        validate_name("graph", graph)?;
        validate_name("source", source)?;
        validate_name("target", target)?;
        let mut writer = self.lock_writer()?;
        let stmt = writer.slot(StmtSlot::GraphEdgeDelete)?;
        stmt.reset()?;
        stmt.bind_text_static(1, graph)?;
        stmt.bind_text_static(2, source)?;
        stmt.bind_text_static(3, target)?;
        stmt.bind_text_static(4, kind)?;
        stmt.step()?;
        stmt.reset()?;
        let deleted = writer.changes() > 0;
        Ok(deleted)
    }

    pub fn graph_list_edges(
        &self,
        graph: &str,
        source: Option<&str>,
        target: Option<&str>,
        kind: Option<&str>,
        limit: u32,
    ) -> Result<Vec<GraphEdge>> {
        validate_name("graph", graph)?;
        let conn = self.read_lock()?;
        let mut stmt = conn.prepare(
            "SELECT source, target, kind, weight FROM _lilia_graph_edges \
             WHERE graph = ?1 \
               AND (?2 IS NULL OR source = ?2) \
               AND (?3 IS NULL OR target = ?3) \
               AND (?4 IS NULL OR kind = ?4) \
             ORDER BY source, target, kind \
             LIMIT ?5;",
        )?;
        stmt.bind_text_static(1, graph)?;
        match source {
            Some(s) => stmt.bind_text_static(2, s)?,
            None => stmt.bind_null(2)?,
        }
        match target {
            Some(t) => stmt.bind_text_static(3, t)?,
            None => stmt.bind_null(3)?,
        }
        match kind {
            Some(k) => stmt.bind_text_static(4, k)?,
            None => stmt.bind_null(4)?,
        }
        stmt.bind_int64(5, normalize_limit(limit))?;

        let mut edges = Vec::new();
        while stmt.step()? {
            edges.push(GraphEdge {
                graph: graph.to_owned(),
                source: stmt.column_text(0).to_string(),
                target: stmt.column_text(1).to_string(),
                kind: stmt.column_text(2).to_string(),
                weight: stmt.column_double(3),
            });
        }
        Ok(edges)
    }

    pub fn graph_reachability(
        &self,
        graph: &str,
        start_nodes: &[String],
        direction: Direction,
    ) -> Result<Vec<String>> {
        validate_name("graph", graph)?;
        if start_nodes.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.read_lock()?;
        let starts_json = serde_json::to_string(start_nodes)
            .map_err(|e| LiliaError::new(ErrorCode::InvalidInput, e.to_string(), false))?;

        let sql = match direction {
            Direction::Incoming => {
                "WITH RECURSIVE reached(node) AS ( \
                    SELECT value FROM json_each(?1) \
                    UNION \
                    SELECT e.source FROM _lilia_graph_edges e \
                    JOIN reached r ON e.target = r.node \
                    WHERE e.graph = ?2 \
                ) \
                SELECT DISTINCT node FROM reached ORDER BY node;"
            }
            Direction::Outgoing => {
                "WITH RECURSIVE reached(node) AS ( \
                    SELECT value FROM json_each(?1) \
                    UNION \
                    SELECT e.target FROM _lilia_graph_edges e \
                    JOIN reached r ON e.source = r.node \
                    WHERE e.graph = ?2 \
                ) \
                SELECT DISTINCT node FROM reached ORDER BY node;"
            }
            Direction::Both => {
                "WITH RECURSIVE reached(node) AS ( \
                    SELECT value FROM json_each(?1) \
                    UNION \
                    SELECT CASE WHEN e.source = r.node THEN e.target ELSE e.source END \
                    FROM _lilia_graph_edges e \
                    JOIN reached r ON e.source = r.node OR e.target = r.node \
                    WHERE e.graph = ?2 \
                ) \
                SELECT DISTINCT node FROM reached ORDER BY node;"
            }
        };

        let mut stmt = conn.prepare(sql)?;
        stmt.bind_text_static(1, &starts_json)?;
        stmt.bind_text_static(2, graph)?;

        let mut nodes = Vec::new();
        while stmt.step()? {
            nodes.push(stmt.column_text(0).to_string());
        }
        Ok(nodes)
    }

    pub fn graph_neighbors(
        &self,
        graph: &str,
        node: &str,
        direction: Direction,
        kind_filter: Option<&str>,
    ) -> Result<Vec<String>> {
        validate_name("graph", graph)?;
        validate_name("node", node)?;
        let conn = self.read_lock()?;
        let sql = match direction {
            Direction::Outgoing => {
                if kind_filter.is_some() {
                    "SELECT DISTINCT target FROM _lilia_graph_edges WHERE graph = ?1 AND source = ?2 AND kind = ?3 ORDER BY target;"
                } else {
                    "SELECT DISTINCT target FROM _lilia_graph_edges WHERE graph = ?1 AND source = ?2 ORDER BY target;"
                }
            }
            Direction::Incoming => {
                if kind_filter.is_some() {
                    "SELECT DISTINCT source FROM _lilia_graph_edges WHERE graph = ?1 AND target = ?2 AND kind = ?3 ORDER BY source;"
                } else {
                    "SELECT DISTINCT source FROM _lilia_graph_edges WHERE graph = ?1 AND target = ?2 ORDER BY source;"
                }
            }
            Direction::Both => {
                if kind_filter.is_some() {
                    "SELECT DISTINCT CASE WHEN source = ?2 THEN target ELSE source END AS neighbor FROM _lilia_graph_edges WHERE graph = ?1 AND (source = ?2 OR target = ?2) AND kind = ?3 ORDER BY neighbor;"
                } else {
                    "SELECT DISTINCT CASE WHEN source = ?2 THEN target ELSE source END AS neighbor FROM _lilia_graph_edges WHERE graph = ?1 AND (source = ?2 OR target = ?2) ORDER BY neighbor;"
                }
            }
        };

        let mut stmt = conn.prepare(sql)?;
        stmt.bind_text_static(1, graph)?;
        stmt.bind_text_static(2, node)?;
        if let Some(kind) = kind_filter {
            stmt.bind_text_static(3, kind)?;
        }

        let mut neighbors = Vec::new();
        while stmt.step()? {
            neighbors.push(stmt.column_text(0).to_string());
        }
        Ok(neighbors)
    }
}
