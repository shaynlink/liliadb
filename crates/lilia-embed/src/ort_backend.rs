//! In-database text embedding backend powered by ONNX Runtime (`ort`).
//!
//! Suitable for Android `NNAPI`, Windows `DirectML`, and quantized ONNX models (~20 MB).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use lilia_core::{ErrorCode, LiliaError};

use crate::{BuiltinModel, Embedder};
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::Tensor as OrtTensor;
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

use crate::TOKENIZER_FILE;

/// Name of the ONNX model file.
pub const ONNX_FILE: &str = "model.onnx";

/// Failure to load or run the ONNX model.
#[derive(Debug, thiserror::Error)]
pub enum OrtError {
    #[error("ONNX model file '{path}' cannot be read: {source}")]
    ModelFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("tokenizer '{path}' cannot be loaded: {message}")]
    Tokenizer { path: PathBuf, message: String },
    #[error("ONNX session error: {0}")]
    Session(#[from] ort::Error),
    #[error("tokenization failed: {0}")]
    Tokenize(String),
}

impl From<OrtError> for LiliaError {
    fn from(error: OrtError) -> Self {
        let code = match &error {
            OrtError::ModelFile { .. } => ErrorCode::Io,
            OrtError::Tokenizer { .. } => ErrorCode::InvalidInput,
            OrtError::Session(_) | OrtError::Tokenize(_) => ErrorCode::Storage,
        };
        Self::new(code, error.to_string(), false)
    }
}

/// Sentence-transformer running on ONNX Runtime (CPU, `NNAPI`, `DirectML`, `CoreML`).
pub struct OrtEmbedder {
    model_id: BuiltinModel,
    /// Running a session takes it mutably: embeddings run one batch at a time.
    session: Mutex<Session>,
    tokenizer: Tokenizer,
}

impl std::fmt::Debug for OrtEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrtEmbedder")
            .field("model", &self.model_id.id())
            .finish_non_exhaustive()
    }
}

impl OrtEmbedder {
    /// Loads an ONNX model from `dir`, which must contain `model.onnx` and `tokenizer.json`.
    ///
    /// # Errors
    /// [`OrtError::ModelFile`] when a file is missing or unreadable, and the ONNX Runtime or
    /// tokenizer error when the model cannot be loaded.
    pub fn load(model: BuiltinModel, dir: impl AsRef<Path>) -> Result<Self, OrtError> {
        let dir = dir.as_ref();
        let model_path = dir.join(ONNX_FILE);
        if !model_path.exists() {
            return Err(OrtError::ModelFile {
                path: model_path.clone(),
                source: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("missing {}", model_path.display()),
                ),
            });
        }

        let tokenizer_path = dir.join(TOKENIZER_FILE);
        let tokenizer_error = |message: String| OrtError::Tokenizer {
            path: tokenizer_path.clone(),
            message,
        };
        std::fs::metadata(&tokenizer_path).map_err(|source| OrtError::ModelFile {
            path: tokenizer_path.clone(),
            source,
        })?;
        let mut tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|error| tokenizer_error(error.to_string()))?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: model.max_tokens(),
                ..TruncationParams::default()
            }))
            .map_err(|error| tokenizer_error(error.to_string()))?;
        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::BatchLongest,
            ..PaddingParams::default()
        }));

        let environment = ort::init().build()?;
        let session = Session::builder(&environment)?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(ort::Error::from)?
            .with_intra_threads(4)
            .map_err(ort::Error::from)?
            .commit_from_file(&model_path)?;

        Ok(Self {
            model_id: model,
            session: Mutex::new(session),
            tokenizer,
        })
    }

    /// Embeds texts into unit-length vectors using ONNX Runtime.
    ///
    /// # Errors
    /// The tokenizer or ONNX Runtime error when a text cannot be encoded or the model fails.
    pub fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, OrtError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|err| OrtError::Tokenize(err.to_string()))?;

        let rows = encodings.len();
        let width = encodings
            .first()
            .map_or(0, |encoding| encoding.get_ids().len());

        let mut input_ids = Vec::with_capacity(rows * width);
        let mut token_type_ids = Vec::with_capacity(rows * width);
        let mut attention_mask = Vec::with_capacity(rows * width);

        for encoding in &encodings {
            for &id in encoding.get_ids() {
                input_ids.push(i64::from(id));
            }
            for &type_id in encoding.get_type_ids() {
                token_type_ids.push(i64::from(type_id));
            }
            for &mask in encoding.get_attention_mask() {
                attention_mask.push(i64::from(mask));
            }
        }

        let input_ids_tensor = OrtTensor::from_array(([rows, width], input_ids))?;
        let token_type_ids_tensor = OrtTensor::from_array(([rows, width], token_type_ids))?;
        let attention_mask_tensor = OrtTensor::from_array(([rows, width], attention_mask.clone()))?;

        // A panic while running poisons the lock, not the session: ONNX Runtime keeps no state
        // between runs.
        let mut session = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        let outputs = session.run(ort::inputs![
            "input_ids" => input_ids_tensor,
            "token_type_ids" => token_type_ids_tensor,
            "attention_mask" => attention_mask_tensor,
        ])?;

        // Extract last_hidden_state (or first output tensor)
        let (_output_shape, output_data) = outputs[0].try_extract_tensor::<f32>()?;
        let dim = self.model_id.dimension() as usize;
        let mut results = Vec::with_capacity(rows);

        // Perform mean-pooling and L2 normalization
        for r in 0..rows {
            let mut pooled = vec![0.0_f32; dim];
            let mut count = 0_u16;
            for t in 0..width {
                if attention_mask[r * width + t] > 0 {
                    count = count.saturating_add(1);
                    let offset = (r * width + t) * dim;
                    for (sum, value) in pooled.iter_mut().zip(&output_data[offset..offset + dim]) {
                        *sum += value;
                    }
                }
            }
            if count > 0 {
                let count = f32::from(count);
                for sum in &mut pooled {
                    *sum /= count;
                }
            }
            // L2 normalize
            let norm = pooled.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
            for value in &mut pooled {
                *value /= norm;
            }
            results.push(pooled);
        }

        Ok(results)
    }
}

impl Embedder for OrtEmbedder {
    fn model(&self) -> BuiltinModel {
        self.model_id
    }

    fn embed(&self, texts: &[&str]) -> lilia_core::Result<Vec<Vec<f32>>> {
        self.embed_texts(texts).map_err(LiliaError::from)
    }
}
