#![allow(clippy::missing_errors_doc)]

use std::collections::{BTreeSet, VecDeque};

use lilia_sqlite_native::StmtSlot;

use crate::storage::{normalize_limit, now_ms, validate_name};
use crate::Database;
use lilia_core::{
    BatchOperation, CollectionKind, Direction, ErrorCode, GraphEdge, GraphNode, LiliaError, Result,
    StorageTier,
};

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
        match self.catalog.tier_for(CollectionKind::Graph, graph) {
            StorageTier::Memory => {
                self.mem_engine.graph_set_node(GraphNode {
                    graph: graph.to_owned(),
                    id: id.to_owned(),
                    kind: kind.to_owned(),
                    metadata: metadata.clone(),
                    doc_space: doc_space.map(ToOwned::to_owned),
                    doc_id: doc_id.map(ToOwned::to_owned),
                })?;
                return Ok(());
            }
            StorageTier::Buffered => {
                return self.ring_core.push_op(BatchOperation::GraphSetNode {
                    graph: graph.to_owned(),
                    id: id.to_owned(),
                    kind: kind.to_owned(),
                    metadata: metadata.clone(),
                    doc_space: doc_space.map(ToOwned::to_owned),
                    doc_id: doc_id.map(ToOwned::to_owned),
                });
            }
            StorageTier::Disk => {}
        }
        self.batch(&[BatchOperation::GraphSetNode {
            graph: graph.to_owned(),
            id: id.to_owned(),
            kind: kind.to_owned(),
            metadata: metadata.clone(),
            doc_space: doc_space.map(ToOwned::to_owned),
            doc_id: doc_id.map(ToOwned::to_owned),
        }])?;
        Ok(())
    }

    pub fn graph_get_node(&self, graph: &str, id: &str) -> Result<Option<GraphNode>> {
        validate_name("graph", graph)?;
        validate_name("id", id)?;
        match self.catalog.tier_for(CollectionKind::Graph, graph) {
            StorageTier::Memory => {
                return Ok(self
                    .mem_engine
                    .graph_nodes
                    .get(graph, id.as_bytes(), now_ms(), true)
                    .map(|snap| snap.value));
            }
            StorageTier::Buffered => {
                self.ring_core.flush()?;
            }
            StorageTier::Disk => {}
        }
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
        let Some(space) = node.doc_space.as_deref() else {
            return Ok(None);
        };
        let doc_id = node.doc_id.as_deref().unwrap_or(id);
        Ok(self.json_get(space, doc_id)?.map(|entry| entry.value()))
    }

    pub fn graph_delete_node(&self, graph: &str, id: &str) -> Result<bool> {
        validate_name("graph", graph)?;
        validate_name("id", id)?;
        match self.catalog.tier_for(CollectionKind::Graph, graph) {
            StorageTier::Memory => {
                return Ok(self.mem_engine.graph_delete_node(graph, id));
            }
            StorageTier::Buffered => {
                self.ring_core.flush()?;
            }
            StorageTier::Disk => {}
        }
        let results = self.batch(&[BatchOperation::GraphDeleteNode {
            graph: graph.to_owned(),
            id: id.to_owned(),
        }])?;
        Ok(results.first().is_some_and(|result| result.deleted))
    }

    pub fn graph_list_nodes(
        &self,
        graph: &str,
        kind_filter: Option<&str>,
        limit: u32,
    ) -> Result<Vec<GraphNode>> {
        validate_name("graph", graph)?;
        match self.catalog.tier_for(CollectionKind::Graph, graph) {
            StorageTier::Memory => {
                let norm_limit = usize::try_from(normalize_limit(limit)).unwrap_or(usize::MAX);
                let scanned =
                    self.mem_engine
                        .graph_nodes
                        .scan(Some(graph), None, usize::MAX, now_ms());
                let mut nodes = Vec::new();
                for (_, _, snap) in scanned {
                    if kind_filter.is_some_and(|k| snap.value.kind != k) {
                        continue;
                    }
                    nodes.push(snap.value);
                    if nodes.len() >= norm_limit {
                        break;
                    }
                }
                return Ok(nodes);
            }
            StorageTier::Buffered => {
                self.ring_core.flush()?;
            }
            StorageTier::Disk => {}
        }
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
        match self.catalog.tier_for(CollectionKind::Graph, graph) {
            StorageTier::Memory => {
                self.mem_engine.graph_add_edge(GraphEdge {
                    graph: graph.to_owned(),
                    source: source.to_owned(),
                    target: target.to_owned(),
                    kind: kind.to_owned(),
                    weight,
                })?;
                return Ok(());
            }
            StorageTier::Buffered => {
                return self.ring_core.push_op(BatchOperation::GraphAddEdge {
                    graph: graph.to_owned(),
                    source: source.to_owned(),
                    target: target.to_owned(),
                    kind: kind.to_owned(),
                    weight,
                });
            }
            StorageTier::Disk => {}
        }
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
        match self.catalog.tier_for(CollectionKind::Graph, graph) {
            StorageTier::Memory => {
                return Ok(self
                    .mem_engine
                    .graph_remove_edge(graph, source, target, kind));
            }
            StorageTier::Buffered => {
                self.ring_core.flush()?;
            }
            StorageTier::Disk => {}
        }
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
        match self.catalog.tier_for(CollectionKind::Graph, graph) {
            StorageTier::Memory => {
                let norm_limit = usize::try_from(normalize_limit(limit)).unwrap_or(usize::MAX);
                let mut edges: Vec<GraphEdge> = self
                    .mem_engine
                    .graph_edges
                    .scan(Some(graph), None, usize::MAX, now_ms())
                    .into_iter()
                    .map(|(_, _, snap)| snap.value)
                    .filter(|e| {
                        source.is_none_or(|s| e.source == s)
                            && target.is_none_or(|t| e.target == t)
                            && kind.is_none_or(|k| e.kind == k)
                    })
                    .collect();
                edges.sort_by(|a, b| {
                    a.source
                        .cmp(&b.source)
                        .then_with(|| a.target.cmp(&b.target))
                        .then_with(|| a.kind.cmp(&b.kind))
                });
                if edges.len() > norm_limit {
                    edges.truncate(norm_limit);
                }
                return Ok(edges);
            }
            StorageTier::Buffered => {
                self.ring_core.flush()?;
            }
            StorageTier::Disk => {}
        }
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
        match self.catalog.tier_for(CollectionKind::Graph, graph) {
            StorageTier::Memory => {
                let edges: Vec<GraphEdge> = self
                    .mem_engine
                    .graph_edges
                    .scan(Some(graph), None, usize::MAX, now_ms())
                    .into_iter()
                    .map(|(_, _, snap)| snap.value)
                    .collect();
                let mut visited = BTreeSet::new();
                let mut queue = VecDeque::new();
                for start in start_nodes {
                    if visited.insert(start.clone()) {
                        queue.push_back(start.clone());
                    }
                }
                while let Some(cur) = queue.pop_front() {
                    for edge in &edges {
                        let next = match direction {
                            Direction::Outgoing | Direction::Both if edge.source == cur => {
                                Some(&edge.target)
                            }
                            Direction::Incoming | Direction::Both if edge.target == cur => {
                                Some(&edge.source)
                            }
                            _ => None,
                        };
                        if let Some(neighbor) = next {
                            if visited.insert(neighbor.clone()) {
                                queue.push_back(neighbor.clone());
                            }
                        }
                    }
                }
                return Ok(visited.into_iter().collect());
            }
            StorageTier::Buffered => {
                self.ring_core.flush()?;
            }
            StorageTier::Disk => {}
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
        match self.catalog.tier_for(CollectionKind::Graph, graph) {
            StorageTier::Memory => {
                let edges =
                    self.mem_engine
                        .graph_edges
                        .scan(Some(graph), None, usize::MAX, now_ms());
                let mut neighbors = BTreeSet::new();
                for (_, _, snap) in edges {
                    let edge = snap.value;
                    if kind_filter.is_some_and(|k| edge.kind != k) {
                        continue;
                    }
                    match direction {
                        Direction::Outgoing if edge.source == node => {
                            neighbors.insert(edge.target);
                        }
                        Direction::Incoming if edge.target == node => {
                            neighbors.insert(edge.source);
                        }
                        Direction::Both => {
                            if edge.source == node {
                                neighbors.insert(edge.target);
                            } else if edge.target == node {
                                neighbors.insert(edge.source);
                            }
                        }
                        _ => {}
                    }
                }
                return Ok(neighbors.into_iter().collect());
            }
            StorageTier::Buffered => {
                self.ring_core.flush()?;
            }
            StorageTier::Disk => {}
        }
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
