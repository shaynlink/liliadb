//! Storage entries, mutations, and batch operations.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::graph::default_edge_weight;
use super::vector::VectorElementType;

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[rkyv(crate = rkyv, derive(Debug, PartialEq, Eq))]
#[serde(rename_all = "snake_case")]
pub enum Durability {
    #[default]
    Durable,
    Balanced,
    Performance,
}

#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(crate = rkyv, derive(Debug))]
pub struct KvEntry {
    pub namespace: String,
    #[serde(with = "serde_bytes")]
    pub key: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub value: Vec<u8>,
    #[serde(with = "crate::version")]
    pub version: u64,
    pub expires_at_ms: Option<i64>,
}

#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(crate = rkyv, derive(Debug))]
pub struct JsonEntry {
    pub space: String,
    pub id: String,
    #[serde(rename = "value")]
    pub doc: crate::json::BinaryDoc,
    #[serde(with = "crate::version")]
    pub version: u64,
}

#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(crate = rkyv, derive(Debug))]
pub struct BitemporalEntry {
    pub space: String,
    pub id: String,
    pub vt_from: i64,
    pub vt_to: i64,
    pub tt_from: i64,
    pub tt_to: i64,
    #[rkyv(with = crate::zero_copy::AsJsonString)]
    pub value: Value,
    #[serde(with = "crate::version")]
    pub version: u64,
}

impl std::ops::Deref for JsonEntry {
    type Target = crate::json::BinaryDoc;

    fn deref(&self) -> &Self::Target {
        &self.doc
    }
}

impl JsonEntry {
    #[must_use]
    pub fn new(
        space: impl Into<String>,
        id: impl Into<String>,
        doc: crate::json::BinaryDoc,
        version: u64,
    ) -> Self {
        Self {
            space: space.into(),
            id: id.into(),
            doc,
            version,
        }
    }

    #[inline]
    #[must_use]
    pub fn value(&self) -> Value {
        self.doc.to_json_value()
    }

    #[inline]
    #[must_use]
    pub fn to_json_value(&self) -> Value {
        self.doc.to_json_value()
    }

    #[inline]
    #[must_use]
    pub fn extract(&self, path: &str) -> Option<crate::json::BinaryValueRef<'_>> {
        self.doc.extract(path)
    }

    #[inline]
    #[must_use]
    pub fn extract_str(&self, path: &str) -> Option<&str> {
        self.doc.extract_str(path)
    }

    #[inline]
    #[must_use]
    pub fn extract_i64(&self, path: &str) -> Option<i64> {
        self.doc.extract_i64(path)
    }

    #[inline]
    #[must_use]
    pub fn extract_f64(&self, path: &str) -> Option<f64> {
        self.doc.extract_f64(path)
    }

    #[inline]
    #[must_use]
    pub fn extract_bool(&self, path: &str) -> Option<bool> {
        self.doc.extract_bool(path)
    }
}

#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(crate = rkyv, derive(Debug))]
#[serde(tag = "model", rename_all = "snake_case")]
pub enum BatchOperation {
    KvSet {
        namespace: String,
        #[serde(with = "serde_bytes")]
        key: Vec<u8>,
        #[serde(with = "serde_bytes")]
        value: Vec<u8>,
        #[serde(default, with = "crate::version::optional")]
        if_version: Option<u64>,
        expires_at_ms: Option<i64>,
    },
    KvDelete {
        namespace: String,
        #[serde(with = "serde_bytes")]
        key: Vec<u8>,
        #[serde(default, with = "crate::version::optional")]
        if_version: Option<u64>,
    },
    JsonPut {
        space: String,
        id: String,
        #[rkyv(with = crate::zero_copy::AsJsonString)]
        value: Value,
        #[serde(default, with = "crate::version::optional")]
        if_version: Option<u64>,
    },
    JsonPutRaw {
        space: String,
        id: String,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
        #[serde(default, with = "crate::version::optional")]
        if_version: Option<u64>,
    },
    JsonDelete {
        space: String,
        id: String,
        #[serde(default, with = "crate::version::optional")]
        if_version: Option<u64>,
    },
    GraphSetNode {
        graph: String,
        id: String,
        kind: String,
        #[rkyv(with = crate::zero_copy::AsJsonString)]
        metadata: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        doc_space: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
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
        #[serde(default = "default_edge_weight")]
        weight: f64,
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
        dim: u32,
        element_type: VectorElementType,
        #[serde(with = "serde_bytes")]
        vector: Vec<u8>,
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
        #[rkyv(with = crate::zero_copy::AsJsonString)]
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
        /// The JSON of the merged `LwwJsonDocument`.
        #[serde(with = "serde_bytes")]
        state: Vec<u8>,
    },
    BitemporalPut {
        space: String,
        id: String,
        #[rkyv(with = crate::zero_copy::AsJsonString)]
        value: Value,
        vt_from: i64,
        vt_to: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_time: Option<i64>,
    },
    BitemporalDelete {
        space: String,
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        vt_from: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        vt_to: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_time: Option<i64>,
    },
}

#[derive(
    Debug, Clone, Default, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
#[rkyv(crate = rkyv, derive(Debug))]
pub struct MutationResult {
    #[serde(default, with = "crate::version::optional")]
    pub version: Option<u64>,
    pub deleted: bool,
}
