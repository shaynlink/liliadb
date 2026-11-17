//! In-database text embeddings: collection configuration and the inference contract.
//!
//! The storage engine never links an inference runtime. It drives an [`Embedder`] supplied when
//! the database is opened; the `lilia-embed` crate provides the built-in implementation.

use serde::{Deserialize, Serialize};

use crate::json::BinaryDoc;
use crate::Result;

/// Embedding models the engine can index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BuiltinModel {
    /// `sentence-transformers/all-MiniLM-L6-v2`: 384 dimensions, 256 word pieces per text.
    #[serde(rename = "all-minilm-l6-v2", alias = "minilm-l6", alias = "minilm_l6")]
    MiniLmL6V2,
}

#[allow(non_upper_case_globals)]
impl BuiltinModel {
    /// Alias for `MiniLM` L6.
    pub const MiniLM_L6: Self = Self::MiniLmL6V2;

    /// Canonical identifier used for catalog storage.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::MiniLmL6V2 => "all-minilm-l6-v2",
        }
    }

    /// Number of components of the vectors the model produces.
    #[must_use]
    pub const fn dimension(self) -> u32 {
        match self {
            Self::MiniLmL6V2 => 384,
        }
    }

    /// Longest input, in tokens, the model accepts; longer texts are truncated.
    #[must_use]
    pub const fn max_tokens(self) -> usize {
        match self {
            Self::MiniLmL6V2 => 256,
        }
    }
}

fn default_target_field() -> String {
    "embedding".to_string()
}

/// Keeps the vector collection named after a JSON collection in sync with one of its text fields.
///
/// Each document is embedded in the background after it commits and the vector is stored under
/// the document id, so the document itself is never rewritten.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoEmbedConfig {
    /// Path of the text field to embed (e.g. `content`, `article.body`).
    pub source_field: String,
    /// Logical target field name for the embedding vector (defaults to "embedding").
    #[serde(default = "default_target_field")]
    pub target_field: String,
    /// Model used for both the documents and the text queries of the collection.
    pub model: BuiltinModel,
}

impl AutoEmbedConfig {
    #[must_use]
    pub fn new(source_field: impl Into<String>, model: BuiltinModel) -> Self {
        Self {
            source_field: source_field.into(),
            target_field: default_target_field(),
            model,
        }
    }

    #[must_use]
    pub fn with_target_field(
        source_field: impl Into<String>,
        target_field: impl Into<String>,
        model: BuiltinModel,
    ) -> Self {
        Self {
            source_field: source_field.into(),
            target_field: target_field.into(),
            model,
        }
    }

    /// Returns the text to embed for `doc`, or `None` when the field is missing, is not a string
    /// or is blank.
    #[must_use]
    pub fn source_text<'a>(&self, doc: &'a BinaryDoc) -> Option<&'a str> {
        doc.extract_str(&self.source_field)
            .filter(|text| !text.trim().is_empty())
    }
}

/// Text embedding backend driven by the storage engine.
pub trait Embedder: Send + Sync + std::fmt::Debug {
    /// Model this backend runs.
    fn model(&self) -> BuiltinModel;

    /// Embeds `texts`, returning one vector of [`BuiltinModel::dimension`] components per text,
    /// in the same order.
    ///
    /// # Errors
    ///
    /// Returns an error when inference fails; the engine retries the affected documents later.
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
}

/// Tuning of the background embedding worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbedOptions {
    /// Documents embedded per inference call.
    pub batch_size: usize,
    /// Failed attempts after which a document is reported as failed instead of retried.
    pub max_attempts: u32,
    /// Delay before the first retry; it doubles after each further failure.
    pub retry_backoff_ms: u64,
}

impl Default for EmbedOptions {
    fn default() -> Self {
        Self {
            batch_size: 32,
            max_attempts: 5,
            retry_backoff_ms: 500,
        }
    }
}

/// Progress of the background embedding of one collection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbedStatus {
    /// Documents waiting for their vector, including those being retried.
    pub pending: u64,
    /// Documents whose embedding failed too many times; rewriting them re-arms the embedding.
    pub failed: u64,
    /// Error of the most recently failed document, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(value: &serde_json::Value) -> BinaryDoc {
        BinaryDoc::from_json_value(value).unwrap()
    }

    #[test]
    fn source_text_reads_nested_string_fields() {
        let config = AutoEmbedConfig::new("article.body", BuiltinModel::MiniLmL6V2);
        let document = doc(&serde_json::json!({"article": {"body": "hello world"}}));
        assert_eq!(config.source_text(&document), Some("hello world"));
    }

    #[test]
    fn source_text_ignores_missing_blank_and_non_string_fields() {
        let config = AutoEmbedConfig::new("content", BuiltinModel::MiniLmL6V2);
        assert_eq!(
            config.source_text(&doc(&serde_json::json!({"title": "x"}))),
            None
        );
        assert_eq!(
            config.source_text(&doc(&serde_json::json!({"content": "  \n"}))),
            None
        );
        assert_eq!(
            config.source_text(&doc(&serde_json::json!({"content": 42}))),
            None
        );
    }

    #[test]
    fn model_round_trips_through_its_identifier() {
        let json = serde_json::to_string(&BuiltinModel::MiniLmL6V2).unwrap();
        assert_eq!(json, format!("\"{}\"", BuiltinModel::MiniLmL6V2.id()));
        let parsed: BuiltinModel = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.dimension(), 384);

        // Alias and alternate serialized formats:
        assert_eq!(BuiltinModel::MiniLM_L6, BuiltinModel::MiniLmL6V2);
        let parsed_alias: BuiltinModel = serde_json::from_str("\"minilm-l6\"").unwrap();
        assert_eq!(parsed_alias, BuiltinModel::MiniLM_L6);
    }

    #[test]
    fn auto_embed_config_target_field_defaults_and_serializes() {
        let config = AutoEmbedConfig {
            source_field: "content".into(),
            target_field: "embedding".into(),
            model: BuiltinModel::MiniLM_L6,
        };
        assert_eq!(config.target_field, "embedding");

        let json = serde_json::json!({
            "source_field": "content",
            "model": "minilm-l6"
        });
        let deserialized: AutoEmbedConfig = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.target_field, "embedding");
        assert_eq!(deserialized.model, BuiltinModel::MiniLM_L6);
    }
}
