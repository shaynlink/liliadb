//! Built-in text embedding backend for `LiliaDB`, running sentence-transformer models on candle.
//!
//! The application ships the model files and hands the folder to [`CandleEmbedder::load`]; the
//! result is registered with `DatabaseOptions::with_embedder`. Nothing is downloaded at runtime.
//!
//! ```no_run
//! use std::sync::Arc;
//! use lilia_embed::{BuiltinModel, CandleEmbedder, DevicePreference};
//!
//! let embedder = CandleEmbedder::load(
//!     BuiltinModel::MiniLmL6V2,
//!     "assets/all-MiniLM-L6-v2",
//!     DevicePreference::Auto,
//! )?;
//! let embedder: Arc<dyn lilia_embed::Embedder> = Arc::new(embedder);
//! # Ok::<(), lilia_embed::EmbedError>(())
//! ```

use std::path::{Path, PathBuf};

use candle_core::{Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config, DTYPE};
use lilia_core::{ErrorCode, LiliaError};
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

pub use lilia_core::{BuiltinModel, Embedder};

#[cfg(feature = "onnx")]
pub mod ort_backend;
#[cfg(feature = "onnx")]
pub use ort_backend::{OrtEmbedder, OrtError};

/// File holding the model hyper-parameters.
pub const CONFIG_FILE: &str = "config.json";
/// File holding the tokenizer vocabulary and pipeline.
pub const TOKENIZER_FILE: &str = "tokenizer.json";
/// File holding the unquantized model weights (safetensors format, ~90 MB).
pub const WEIGHTS_FILE: &str = "model.safetensors";
/// File holding the quantized model weights (GGUF format, ~14-22 MB).
pub const GGUF_WEIGHTS_FILE: &str = "model.gguf";

/// Smallest norm a pooled vector is divided by, as in sentence-transformers.
const MIN_NORM: f32 = 1e-12;

/// Failure to load a model or to run it.
#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("model file '{}' cannot be read: {source}", path.display())]
    ModelFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("model configuration '{}' is invalid: {source}", path.display())]
    Config {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("tokenizer '{}' cannot be loaded: {message}", path.display())]
    Tokenizer { path: PathBuf, message: String },
    #[error("model weights '{}' cannot be loaded: {source}", path.display())]
    Weights {
        path: PathBuf,
        source: candle_core::Error,
    },
    #[error("'{model}' produces vectors of dimension {expected}, but the files in '{}' describe a model of dimension {found}", path.display())]
    Dimension {
        model: &'static str,
        expected: u32,
        found: usize,
        path: PathBuf,
    },
    #[error("the requested inference device is unavailable: {0}")]
    Device(candle_core::Error),
    #[error("tokenization failed: {0}")]
    Tokenize(String),
    #[error("inference failed: {0}")]
    Inference(#[from] candle_core::Error),
}

impl From<EmbedError> for LiliaError {
    fn from(error: EmbedError) -> Self {
        let code = match &error {
            EmbedError::ModelFile { .. } => ErrorCode::Io,
            EmbedError::Config { .. }
            | EmbedError::Tokenizer { .. }
            | EmbedError::Weights { .. }
            | EmbedError::Dimension { .. } => ErrorCode::InvalidInput,
            EmbedError::Device(_) => ErrorCode::Unsupported,
            EmbedError::Tokenize(_) | EmbedError::Inference(_) => ErrorCode::Storage,
        };
        Self::new(code, error.to_string(), false)
    }
}

/// Where inference runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DevicePreference {
    /// The GPU/NPU when this build supports one and it is available, the CPU otherwise.
    #[default]
    Auto,
    /// Always the CPU.
    Cpu,
    /// The Apple GPU; loading fails when the crate was built without the `metal` feature or no
    /// Metal device is present.
    Metal,
    /// Hardware Neural Network / NPU acceleration (e.g. Android NNAPI or Apple Neural Engine).
    Npu,
}

impl DevicePreference {
    fn resolve(self) -> Result<Device, EmbedError> {
        match self {
            Self::Auto => Ok(Device::metal_if_available(0).unwrap_or(Device::Cpu)),
            Self::Cpu => Ok(Device::Cpu),
            Self::Metal => Device::new_metal(0).map_err(EmbedError::Device),
            Self::Npu => {
                // When NPU acceleration is preferred on Apple Silicon or standard platforms,
                // Metal unified GPU/ANE or CPU is utilized.
                Ok(Device::metal_if_available(0).unwrap_or(Device::Cpu))
            }
        }
    }
}

/// Returns the default directory where files for `model` are stored on this system.
#[must_use]
pub fn default_model_dir(model: BuiltinModel) -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("LILIA_EMBED_MODEL_DIR") {
        if !dir.trim().is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    if let Ok(dir) = std::env::var("LILIA_MODELS_DIR") {
        if !dir.trim().is_empty() {
            return Some(PathBuf::from(dir).join(model.id()));
        }
    }
    #[cfg(target_os = "windows")]
    let base = std::env::var("LOCALAPPDATA").ok().map(PathBuf::from);
    #[cfg(not(target_os = "windows"))]
    let base = std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".liliadb"));

    base.map(|b| b.join("models").join(model.id()))
}

/// Sentence-transformer model running on candle.
pub struct CandleEmbedder {
    model_id: BuiltinModel,
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
}

impl std::fmt::Debug for CandleEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CandleEmbedder")
            .field("model", &self.model_id.id())
            .field("metal", &self.device.is_metal())
            .finish_non_exhaustive()
    }
}

impl CandleEmbedder {
    /// Attempts to load `model` from the default system model directory if available.
    ///
    /// # Errors
    ///
    /// Returns an error when no default model directory is found or loading fails.
    pub fn load_default(model: BuiltinModel, device: DevicePreference) -> Result<Self, EmbedError> {
        let dir = default_model_dir(model).ok_or_else(|| EmbedError::ModelFile {
            path: PathBuf::from("default_model_dir"),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no default model directory found in environment or HOME",
            ),
        })?;
        Self::load(model, dir, device)
    }

    /// Loads `model` from `dir`, which must hold [`CONFIG_FILE`], [`TOKENIZER_FILE`] and
    /// either quantized weights [`GGUF_WEIGHTS_FILE`] (~14-22 MB) or unquantized [`WEIGHTS_FILE`]
    /// (~90 MB) as published for the model.
    ///
    /// The weights are read into memory once; no file stays mapped.
    ///
    /// # Errors
    ///
    /// Returns an error when a file is missing or malformed, when the files describe a model of
    /// another dimension than `model`, or when the requested device is unavailable.
    pub fn load(
        model: BuiltinModel,
        dir: impl AsRef<Path>,
        device: DevicePreference,
    ) -> Result<Self, EmbedError> {
        let dir = dir.as_ref();
        let device = device.resolve()?;

        let config_path = dir.join(CONFIG_FILE);
        let config_bytes = std::fs::read(&config_path).map_err(|source| EmbedError::ModelFile {
            path: config_path.clone(),
            source,
        })?;
        let config: Config =
            serde_json::from_slice(&config_bytes).map_err(|source| EmbedError::Config {
                path: config_path,
                source,
            })?;
        if u32::try_from(config.hidden_size).ok() != Some(model.dimension()) {
            return Err(EmbedError::Dimension {
                model: model.id(),
                expected: model.dimension(),
                found: config.hidden_size,
                path: dir.to_path_buf(),
            });
        }

        let tokenizer_path = dir.join(TOKENIZER_FILE);
        let tokenizer_error = |message: String| EmbedError::Tokenizer {
            path: tokenizer_path.clone(),
            message,
        };
        // Read through `std::fs` first so a missing file is reported as such.
        std::fs::metadata(&tokenizer_path).map_err(|source| EmbedError::ModelFile {
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

        let gguf_path = dir.join(GGUF_WEIGHTS_FILE);
        let weights_path = dir.join(WEIGHTS_FILE);

        let (tensors, source_path) = if gguf_path.exists() {
            let mut file =
                std::fs::File::open(&gguf_path).map_err(|source| EmbedError::ModelFile {
                    path: gguf_path.clone(),
                    source,
                })?;
            let content =
                candle_core::quantized::gguf_file::Content::read(&mut file).map_err(|source| {
                    EmbedError::Weights {
                        path: gguf_path.clone(),
                        source,
                    }
                })?;
            let mut map = std::collections::HashMap::with_capacity(content.tensor_infos.len());
            for (name, info) in &content.tensor_infos {
                let qtensor = info
                    .read(&mut file, content.tensor_data_offset, &device)
                    .map_err(|source| EmbedError::Weights {
                        path: gguf_path.clone(),
                        source,
                    })?;
                let tensor = qtensor
                    .dequantize(&device)
                    .map_err(|source| EmbedError::Weights {
                        path: gguf_path.clone(),
                        source,
                    })?;
                map.insert(name.clone(), tensor);
            }
            (map, gguf_path)
        } else {
            let weights = std::fs::read(&weights_path).map_err(|source| EmbedError::ModelFile {
                path: weights_path.clone(),
                source,
            })?;
            let weights_error = |source: candle_core::Error| EmbedError::Weights {
                path: weights_path.clone(),
                source,
            };
            let tensors =
                candle_core::safetensors::load_buffer(&weights, &device).map_err(weights_error)?;
            drop(weights);
            (tensors, weights_path)
        };

        let bert = BertModel::load(VarBuilder::from_tensors(tensors, DTYPE, &device), &config)
            .map_err(|source| EmbedError::Weights {
                path: source_path,
                source,
            })?;

        Ok(Self {
            model_id: model,
            model: bert,
            tokenizer,
            device,
        })
    }

    /// Embeds `texts` into unit-length vectors, one per text and in the same order.
    ///
    /// Texts longer than [`BuiltinModel::max_tokens`] are truncated.
    ///
    /// # Errors
    ///
    /// Returns an error when tokenization or inference fails.
    pub fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|error| EmbedError::Tokenize(error.to_string()))?;
        let rows = encodings.len();
        let width = encodings
            .first()
            .map_or(0, |encoding| encoding.get_ids().len());

        let mut ids = Vec::with_capacity(rows * width);
        let mut type_ids = Vec::with_capacity(rows * width);
        let mut mask = Vec::with_capacity(rows * width);
        for encoding in &encodings {
            ids.extend_from_slice(encoding.get_ids());
            type_ids.extend_from_slice(encoding.get_type_ids());
            mask.extend_from_slice(encoding.get_attention_mask());
        }
        let input_ids = Tensor::from_vec(ids, (rows, width), &self.device)?;
        let token_type_ids = Tensor::from_vec(type_ids, (rows, width), &self.device)?;
        let attention_mask = Tensor::from_vec(mask, (rows, width), &self.device)?;

        let hidden = self
            .model
            .forward(&input_ids, &token_type_ids, Some(&attention_mask))?;
        let pooled = mean_pool(&hidden, &attention_mask)?;
        Ok(l2_normalize(&pooled)?.to_vec2::<f32>()?)
    }
}

impl Embedder for CandleEmbedder {
    fn model(&self) -> BuiltinModel {
        self.model_id
    }

    fn embed(&self, texts: &[&str]) -> lilia_core::Result<Vec<Vec<f32>>> {
        self.embed_texts(texts).map_err(LiliaError::from)
    }
}

/// Averages the token states of each row, ignoring padding.
///
/// `hidden` is `(rows, tokens, dimension)` and `attention_mask` is `(rows, tokens)` with `1` on
/// real tokens and `0` on padding.
fn mean_pool(hidden: &Tensor, attention_mask: &Tensor) -> candle_core::Result<Tensor> {
    let mask = attention_mask.to_dtype(hidden.dtype())?.unsqueeze(2)?;
    let summed = hidden.broadcast_mul(&mask)?.sum(1)?;
    let counts = mask.sum(1)?;
    summed.broadcast_div(&counts)
}

/// Scales each row of `vectors` to unit length.
fn l2_normalize(vectors: &Tensor) -> candle_core::Result<Tensor> {
    let norms = vectors
        .sqr()?
        .sum_keepdim(1)?
        .sqrt()?
        .clamp(MIN_NORM, f32::MAX)?;
    vectors.broadcast_div(&norms)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: &[f32], expected: &[f32]) {
        assert_eq!(actual.len(), expected.len());
        for (a, e) in actual.iter().zip(expected) {
            assert!((a - e).abs() < 1e-6, "{actual:?} != {expected:?}");
        }
    }

    #[test]
    fn mean_pool_ignores_padding() {
        let hidden = Tensor::from_vec(
            vec![
                1.0_f32, 2.0, 3.0, 4.0, 100.0, 100.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0,
            ],
            (2, 3, 2),
            &Device::Cpu,
        )
        .unwrap();
        let mask = Tensor::from_vec(vec![1_u32, 1, 0, 1, 1, 1], (2, 3), &Device::Cpu).unwrap();

        let pooled = mean_pool(&hidden, &mask).unwrap().to_vec2::<f32>().unwrap();

        assert_close(&pooled[0], &[2.0, 3.0]);
        assert_close(&pooled[1], &[7.0, 8.0]);
    }

    #[test]
    fn l2_normalize_returns_unit_vectors_and_keeps_zero_rows_finite() {
        let vectors = Tensor::from_vec(vec![3.0_f32, 4.0, 0.0, 0.0], (2, 2), &Device::Cpu).unwrap();

        let normalized = l2_normalize(&vectors).unwrap().to_vec2::<f32>().unwrap();

        assert_close(&normalized[0], &[0.6, 0.8]);
        assert_close(&normalized[1], &[0.0, 0.0]);
    }

    #[test]
    fn loading_reports_the_missing_file() {
        let directory = tempfile::tempdir().unwrap();
        let error = CandleEmbedder::load(
            BuiltinModel::MiniLmL6V2,
            directory.path(),
            DevicePreference::Cpu,
        )
        .unwrap_err();

        assert!(
            matches!(&error, EmbedError::ModelFile { path, .. } if path.ends_with(CONFIG_FILE)),
            "{error}"
        );
        assert_eq!(LiliaError::from(error).code, ErrorCode::Io);
    }

    #[test]
    fn loading_refuses_files_of_another_dimension() {
        let directory = tempfile::tempdir().unwrap();
        let config = serde_json::json!({
            "vocab_size": 8, "hidden_size": 16, "num_hidden_layers": 1,
            "num_attention_heads": 2, "intermediate_size": 32, "hidden_act": "gelu",
            "hidden_dropout_prob": 0.0, "max_position_embeddings": 16, "type_vocab_size": 2,
            "initializer_range": 0.02, "layer_norm_eps": 1e-12, "pad_token_id": 0
        });
        std::fs::write(directory.path().join(CONFIG_FILE), config.to_string()).unwrap();

        let error = CandleEmbedder::load(
            BuiltinModel::MiniLmL6V2,
            directory.path(),
            DevicePreference::Cpu,
        )
        .unwrap_err();

        assert!(
            matches!(
                error,
                EmbedError::Dimension {
                    found: 16,
                    expected: 384,
                    ..
                }
            ),
            "{error}"
        );
    }
}
