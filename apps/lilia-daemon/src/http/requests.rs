//! The JSON bodies of the gateway's routes.

use lilia_core::{VectorElementType, VectorMetric};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct KvGetRequest {
    pub namespace: String,
    pub key: String,
    #[serde(default)]
    pub encoding: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct KvSetRequest {
    pub namespace: String,
    pub key: String,
    pub value: String,
    #[serde(default)]
    pub if_version: Option<u64>,
    #[serde(default)]
    pub expires_at_ms: Option<i64>,
    #[serde(default)]
    pub encoding: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct KvDeleteRequest {
    pub namespace: String,
    pub key: String,
    #[serde(default)]
    pub if_version: Option<u64>,
    #[serde(default)]
    pub encoding: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct KvScanRequest {
    pub namespace: String,
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub encoding: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct JsonGetRequest {
    pub space: String,
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct JsonPutRequest {
    pub space: String,
    pub id: String,
    pub value: Value,
    #[serde(default)]
    pub if_version: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct JsonDeleteRequest {
    pub space: String,
    pub id: String,
    #[serde(default)]
    pub if_version: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct JsonScanRequest {
    pub space: String,
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct JsonCreateIndexRequest {
    pub space: String,
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub filter: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct JsonDropIndexRequest {
    pub space: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct JsonListIndexesRequest {
    pub space: String,
}

#[derive(Debug, Deserialize)]
pub struct JsonScanByIndexRequest {
    pub space: String,
    pub index_name: String,
    pub value: String,
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct JsonFindRequest {
    pub space: String,
    pub query: serde_json::Map<String, Value>,
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct GraphNodeRequest {
    pub graph: String,
    pub id: String,
    #[serde(default = "default_node_kind")]
    pub kind: String,
    #[serde(default)]
    pub metadata: Option<Value>,
}

fn default_node_kind() -> String {
    "default".into()
}

#[derive(Debug, Deserialize)]
pub struct GraphEdgeRequest {
    pub graph: String,
    pub source: String,
    pub target: String,
    #[serde(default = "default_edge_kind")]
    pub kind: String,
    #[serde(default)]
    pub weight: Option<f64>,
}

fn default_edge_kind() -> String {
    "edge".into()
}

#[derive(Debug, Deserialize)]
pub struct GraphNeighborsRequest {
    pub graph: String,
    pub node: String,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GraphReachabilityRequest {
    pub graph: String,
    pub start_nodes: Vec<String>,
    #[serde(default)]
    pub direction: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FtsIndexRequest {
    pub collection: String,
    pub doc_id: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct FtsSearchRequest {
    pub collection: String,
    pub query: String,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct FtsSuggestRequest {
    pub collection: String,
    pub prefix: String,
    #[serde(default)]
    pub max_distance: Option<u8>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct FtsFuzzySearchRequest {
    pub collection: String,
    pub query: String,
    #[serde(default)]
    pub max_distance: Option<u8>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct FtsPhoneticRequest {
    pub collection: String,
    pub query: String,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// A write of a batch request; keys, values and vectors in readable forms.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HttpBatchOperation {
    KvSet {
        #[serde(default, alias = "space")]
        namespace: String,
        key: String,
        value: String,
        #[serde(default)]
        if_version: Option<u64>,
        #[serde(default)]
        expires_at_ms: Option<i64>,
        #[serde(default)]
        encoding: Option<String>,
    },
    KvDelete {
        #[serde(default, alias = "space")]
        namespace: String,
        key: String,
        #[serde(default)]
        if_version: Option<u64>,
        #[serde(default)]
        encoding: Option<String>,
    },
    JsonPut {
        space: String,
        id: String,
        value: Value,
        #[serde(default)]
        if_version: Option<u64>,
    },
    JsonDelete {
        space: String,
        id: String,
        #[serde(default)]
        if_version: Option<u64>,
    },
    GraphSetNode {
        graph: String,
        id: String,
        kind: String,
        #[serde(default)]
        metadata: Option<Value>,
        #[serde(default)]
        doc_space: Option<String>,
        #[serde(default)]
        doc_id: Option<String>,
    },
    GraphDeleteNode {
        graph: String,
        id: String,
    },
    GraphAddEdge {
        graph: String,
        source: String,
        target: String,
        kind: String,
        #[serde(default)]
        weight: Option<f64>,
    },
    GraphRemoveEdge {
        graph: String,
        source: String,
        target: String,
        kind: String,
    },
    FtsIndex {
        collection: String,
        doc_id: String,
        content: String,
    },
    FtsDelete {
        collection: String,
        doc_id: String,
    },
    VectorSet {
        collection: String,
        id: String,
        #[serde(default)]
        element_type: Option<VectorElementType>,
        vector: Vec<f32>,
    },
    VectorDelete {
        collection: String,
        id: String,
    },
    CrdtPnCounterIncrement {
        space: String,
        id: String,
        actor_id: String,
        amount: u64,
    },
    CrdtPnCounterDecrement {
        space: String,
        id: String,
        actor_id: String,
        amount: u64,
    },
    CrdtOrSetAdd {
        space: String,
        id: String,
        actor_id: String,
        element: String,
    },
    CrdtOrSetRemove {
        space: String,
        id: String,
        element: String,
        #[serde(default)]
        timestamp_ms: Option<i64>,
    },
    CrdtJsonSetField {
        space: String,
        id: String,
        actor_id: String,
        path: String,
        value: Value,
        #[serde(default)]
        timestamp_ms: Option<i64>,
    },
    CrdtJsonDeleteField {
        space: String,
        id: String,
        actor_id: String,
        path: String,
        #[serde(default)]
        timestamp_ms: Option<i64>,
    },
    CrdtJsonMerge {
        space: String,
        id: String,
        #[serde(with = "serde_bytes")]
        state: Vec<u8>,
    },
}

#[derive(Debug, Deserialize)]
pub struct VectorSetRequest {
    pub collection: String,
    pub id: String,
    #[serde(default)]
    pub element_type: Option<VectorElementType>,
    pub vector: Vec<f32>,
}

#[derive(Debug, Deserialize)]
pub struct VectorGetRequest {
    pub collection: String,
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct VectorSearchRequest {
    pub collection: String,
    pub vector: Vec<f32>,
    #[serde(default)]
    pub element_type: Option<VectorElementType>,
    #[serde(default)]
    pub metric: Option<VectorMetric>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct HybridSearchRequest {
    pub collection: String,
    #[serde(default)]
    pub fts_query: Option<String>,
    #[serde(default)]
    pub vector: Option<Vec<f32>>,
    #[serde(default)]
    pub element_type: Option<VectorElementType>,
    #[serde(default)]
    pub metric: Option<VectorMetric>,
    #[serde(default)]
    pub rrf_k: Option<u32>,
    #[serde(default)]
    pub fts_weight: Option<f64>,
    #[serde(default)]
    pub vector_weight: Option<f64>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct BatchRequest {
    pub operations: Vec<HttpBatchOperation>,
}

#[derive(Debug, Deserialize)]
pub struct SqlRequest {
    pub query: String,
    #[serde(default)]
    pub params: Vec<Value>,
}
