//! Exercises the whole loading and inference path on a one-layer BERT generated on the fly, so the
//! suite needs no model download. The weights are arbitrary: these tests check the mechanics
//! (tokenization, padding, pooling, normalisation, engine wiring), not semantic quality, which
//! `real_model.rs` covers with the published weights.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use candle_core::{Device, Tensor};
use candle_nn::{VarBuilder, VarMap};
use candle_transformers::models::bert::{BertModel, Config, DTYPE};
use lilia_core::{AutoEmbedConfig, BatchOperation, CollectionOptions};
use lilia_embed::{
    BuiltinModel, CandleEmbedder, DevicePreference, Embedder, CONFIG_FILE, TOKENIZER_FILE,
    WEIGHTS_FILE,
};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

const MODEL: BuiltinModel = BuiltinModel::MiniLmL6V2;
const WORDS: [&str; 12] = [
    "kittens", "purr", "and", "chase", "mice", "central", "banks", "raise", "interest", "rates",
    "puppies", "bark",
];

fn write_tokenizer(dir: &Path) {
    let specials = ["[PAD]", "[CLS]", "[SEP]", "[UNK]"];
    let vocab: serde_json::Map<String, serde_json::Value> = specials
        .iter()
        .chain(WORDS.iter())
        .enumerate()
        .map(|(id, token)| ((*token).to_owned(), json!(id)))
        .collect();
    let added: Vec<_> = specials
        .iter()
        .enumerate()
        .map(|(id, token)| {
            json!({
                "id": id, "content": token, "single_word": false, "lstrip": false,
                "rstrip": false, "normalized": false, "special": true
            })
        })
        .collect();
    let tokenizer = json!({
        "version": "1.0",
        "truncation": null,
        "padding": null,
        "added_tokens": added,
        "normalizer": {
            "type": "BertNormalizer", "clean_text": true, "handle_chinese_chars": true,
            "strip_accents": null, "lowercase": true
        },
        "pre_tokenizer": { "type": "BertPreTokenizer" },
        "post_processor": { "type": "BertProcessing", "sep": ["[SEP]", 2], "cls": ["[CLS]", 1] },
        "decoder": { "type": "WordPiece", "prefix": "##", "cleanup": true },
        "model": {
            "type": "WordPiece", "unk_token": "[UNK]", "continuing_subword_prefix": "##",
            "max_input_chars_per_word": 100, "vocab": vocab
        }
    });
    std::fs::write(dir.join(TOKENIZER_FILE), tokenizer.to_string()).unwrap();
}

/// Writes a one-layer model of the dimension of `MODEL` with deterministic weights.
fn write_model(dir: &Path) {
    let config = json!({
        "vocab_size": WORDS.len() + 4,
        "hidden_size": MODEL.dimension(),
        "num_hidden_layers": 1,
        "num_attention_heads": 6,
        "intermediate_size": 64,
        "hidden_act": "gelu",
        "hidden_dropout_prob": 0.0,
        "max_position_embeddings": MODEL.max_tokens(),
        "type_vocab_size": 2,
        "initializer_range": 0.02,
        "layer_norm_eps": 1e-12,
        "pad_token_id": 0,
        "model_type": "bert"
    });
    std::fs::write(dir.join(CONFIG_FILE), config.to_string()).unwrap();
    write_tokenizer(dir);

    let parsed: Config = serde_json::from_value(config).unwrap();
    let varmap = VarMap::new();
    BertModel::load(
        VarBuilder::from_varmap(&varmap, DTYPE, &Device::Cpu),
        &parsed,
    )
    .unwrap();

    let mut variables: Vec<_> = varmap
        .data()
        .lock()
        .unwrap()
        .iter()
        .map(|(name, variable)| (name.clone(), variable.clone()))
        .collect();
    variables.sort_by(|a, b| a.0.cmp(&b.0));
    let mut state = 0x9E37_79B9_u32;
    for (_, variable) in variables {
        let values: Vec<f32> = (0..variable.elem_count())
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                f32::from(u16::try_from(state >> 16).unwrap()) / 65_535.0 - 0.5
            })
            .collect();
        variable
            .set(&Tensor::from_vec(values, variable.shape(), &Device::Cpu).unwrap())
            .unwrap();
    }
    varmap.save(dir.join(WEIGHTS_FILE)).unwrap();
}

fn embedder() -> (tempfile::TempDir, CandleEmbedder) {
    let directory = tempfile::tempdir().unwrap();
    write_model(directory.path());
    let embedder = CandleEmbedder::load(MODEL, directory.path(), DevicePreference::Cpu).unwrap();
    (directory, embedder)
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[test]
fn vectors_have_the_model_dimension_and_unit_length() {
    let (_directory, embedder) = embedder();

    let vectors = embedder
        .embed_texts(&["kittens purr", "central banks raise interest rates"])
        .unwrap();

    assert_eq!(vectors.len(), 2);
    for vector in &vectors {
        assert_eq!(vector.len(), MODEL.dimension() as usize);
        assert!((cosine(vector, vector) - 1.0).abs() < 1e-4);
    }
    assert!(cosine(&vectors[0], &vectors[1]) < 0.999);
    assert_eq!(embedder.embed_texts(&[]).unwrap(), Vec::<Vec<f32>>::new());
}

#[test]
fn a_text_embeds_the_same_alone_or_padded_in_a_batch() {
    let (_directory, embedder) = embedder();

    let alone = embedder.embed_texts(&["kittens purr"]).unwrap().remove(0);
    let batched = embedder
        .embed_texts(&[
            "central banks raise interest rates and chase mice",
            "kittens purr",
        ])
        .unwrap()
        .remove(1);

    assert!(cosine(&alone, &batched) > 0.9999);
}

#[test]
fn texts_longer_than_the_model_window_are_truncated() {
    let (_directory, embedder) = embedder();
    let long = "kittens purr ".repeat(400);

    let vectors = embedder.embed_texts(&[&long]).unwrap();

    assert!((cosine(&vectors[0], &vectors[0]) - 1.0).abs() < 1e-4);
}

#[test]
fn the_engine_embeds_documents_and_queries_with_the_candle_backend() {
    let (_model, embedder) = embedder();
    let embedder: Arc<dyn Embedder> = Arc::new(embedder);
    let directory = tempfile::tempdir().unwrap();
    let database = Database::open(
        DatabaseOptions::durable(directory.path().join("app.lilia")).with_embedder(embedder),
    )
    .unwrap();
    database
        .json()
        .create_collection(
            "articles",
            CollectionOptions {
                auto_embed: Some(AutoEmbedConfig::new("content", MODEL)),
                ..CollectionOptions::default()
            },
        )
        .unwrap();
    let documents = [
        ("cats", "kittens purr and chase mice"),
        ("rates", "central banks raise interest rates"),
        ("dogs", "puppies bark"),
    ];
    for (id, content) in documents {
        database
            .batch(&[BatchOperation::JsonPut {
                space: "articles".into(),
                id: id.into(),
                value: json!({ "content": content }),
                if_version: None,
            }])
            .unwrap();
    }

    database
        .wait_for_embeddings(Duration::from_secs(60))
        .unwrap();

    for (id, content) in documents {
        let hits = database.vector_search_text("articles", content, 1).unwrap();
        assert_eq!(hits[0].id, id);
        assert!(hits[0].distance < 1e-3, "{id}: {}", hits[0].distance);
    }
}

#[test]
fn collection_creation_with_target_field_and_minilm_l6_model() {
    let (_model, embedder) = embedder();
    let embedder: Arc<dyn Embedder> = Arc::new(embedder);
    let directory = tempfile::tempdir().unwrap();
    let database = Database::open(
        DatabaseOptions::durable(directory.path().join("app.lilia")).with_embedder(embedder),
    )
    .unwrap();

    database
        .json()
        .create_collection(
            "articles",
            CollectionOptions {
                auto_embed: Some(AutoEmbedConfig {
                    source_field: "content".into(),
                    target_field: "embedding".into(),
                    model: BuiltinModel::MiniLM_L6,
                }),
                ..CollectionOptions::default()
            },
        )
        .unwrap();

    database
        .batch(&[BatchOperation::JsonPut {
            space: "articles".into(),
            id: "a1".into(),
            value: json!({ "content": "kittens purr and chase mice" }),
            if_version: None,
        }])
        .unwrap();

    database
        .wait_for_embeddings(Duration::from_secs(60))
        .unwrap();

    let hits = database
        .vector_search_text("articles", "kittens purr", 1)
        .unwrap();
    assert_eq!(hits[0].id, "a1");
}
