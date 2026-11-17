//! Semantic checks against the published `all-MiniLM-L6-v2` weights.
//!
//! The model is not vendored: download `config.json`, `tokenizer.json` and `model.safetensors`
//! from `sentence-transformers/all-MiniLM-L6-v2`, then run
//! `LILIA_EMBED_MODEL_DIR=<folder> cargo test -p lilia-embed -- --ignored`.

use lilia_embed::{BuiltinModel, CandleEmbedder, DevicePreference};

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[test]
#[ignore = "needs the all-MiniLM-L6-v2 files; set LILIA_EMBED_MODEL_DIR"]
fn paraphrases_are_closer_than_unrelated_sentences() {
    let dir = std::env::var("LILIA_EMBED_MODEL_DIR")
        .expect("LILIA_EMBED_MODEL_DIR must point at the all-MiniLM-L6-v2 files");
    let embedder =
        CandleEmbedder::load(BuiltinModel::MiniLmL6V2, dir, DevicePreference::Auto).unwrap();

    let vectors = embedder
        .embed_texts(&[
            "A man is eating food.",
            "Someone is having a meal.",
            "The stock market fell sharply on Monday.",
        ])
        .unwrap();

    for vector in &vectors {
        assert_eq!(vector.len(), 384);
        assert!((cosine(vector, vector) - 1.0).abs() < 1e-4);
    }
    let paraphrase = cosine(&vectors[0], &vectors[1]);
    let unrelated = cosine(&vectors[0], &vectors[2]);
    assert!(paraphrase > 0.5, "paraphrase similarity {paraphrase}");
    assert!(unrelated < 0.2, "unrelated similarity {unrelated}");
}
