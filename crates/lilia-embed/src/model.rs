//! The models this crate runs, and the contract of an embedding backend.

use lilia_core::Result;

/// The sentence-transformer models this crate runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinModel {
    /// `sentence-transformers/all-MiniLM-L6-v2`: 384 dimensions, 256 word pieces per text.
    MiniLmL6V2,
}

impl BuiltinModel {
    /// The model's identifier, also the name of its folder.
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

/// A text embedding backend.
pub trait Embedder: Send + Sync + std::fmt::Debug {
    /// Model this backend runs.
    fn model(&self) -> BuiltinModel;

    /// Embeds `texts`, returning one vector of [`BuiltinModel::dimension`] components per text,
    /// in the same order.
    ///
    /// # Errors
    ///
    /// Returns an error when inference fails.
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
}
