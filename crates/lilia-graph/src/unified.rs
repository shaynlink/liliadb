use lilia_core::{GraphNode, Result};
use lilia_storage_sqlite::Database;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedNode {
    pub id: String,
    pub kind: String,
    pub metadata: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_space: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<serde_json::Value>,
}

impl UnifiedNode {
    #[must_use]
    pub fn new(node: GraphNode, document: Option<serde_json::Value>) -> Self {
        Self {
            id: node.id,
            kind: node.kind,
            metadata: node.metadata,
            doc_space: node.doc_space,
            doc_id: node.doc_id,
            document,
        }
    }

    #[must_use]
    pub fn doc(&self) -> Option<&serde_json::Value> {
        self.document.as_ref()
    }

    #[must_use]
    pub fn get_value<'a>(&'a self, field: &str) -> Option<&'a serde_json::Value> {
        if let Some(doc) = &self.document {
            if let Some(val) = doc.get(field) {
                return Some(val);
            }
            if let Some(val) = doc.pointer(field) {
                return Some(val);
            }
        }
        if let Some(val) = self.metadata.get(field) {
            return Some(val);
        }
        self.metadata.pointer(field)
    }

    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

pub fn resolve_node_document(db: &Database, node: &GraphNode) -> Result<Option<serde_json::Value>> {
    // 1. Explicit doc_space
    if let Some(ref space) = node.doc_space {
        let doc_id = node.doc_id.as_deref().unwrap_or(&node.id);
        return Ok(db.json_get(space, doc_id)?.map(|e| e.value));
    }

    // 2. Cascade resolution: fallback to node.kind if space exists and has matching id
    if let Ok(Some(entry)) = db.json_get(&node.kind, &node.id) {
        return Ok(Some(entry.value));
    }

    Ok(None)
}
