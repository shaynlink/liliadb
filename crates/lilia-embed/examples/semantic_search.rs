//! Semantic search without any embedding code in the application.
//!
//! `cargo run -p lilia-embed --example semantic_search -- <folder with the all-MiniLM-L6-v2 files>`

use std::sync::Arc;
use std::time::Duration;

use lilia_core::{AutoEmbedConfig, BatchOperation, CollectionOptions};
use lilia_embed::{BuiltinModel, CandleEmbedder, DevicePreference, Embedder};
use lilia_storage_sqlite::{Database, DatabaseOptions};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model_dir = std::env::args()
        .nth(1)
        .ok_or("usage: semantic_search <folder with the all-MiniLM-L6-v2 files>")?;
    let embedder: Arc<dyn Embedder> = Arc::new(CandleEmbedder::load(
        BuiltinModel::MiniLmL6V2,
        model_dir,
        DevicePreference::Auto,
    )?);

    let directory = tempfile::tempdir()?;
    let db = Database::open(
        DatabaseOptions::durable(directory.path().join("articles.lilia")).with_embedder(embedder),
    )?;

    db.json().create_collection(
        "articles",
        CollectionOptions {
            auto_embed: Some(AutoEmbedConfig::new("content", BuiltinModel::MiniLmL6V2)),
            ..CollectionOptions::default()
        },
    )?;

    let articles = [
        (
            "pasta",
            "Boil the spaghetti until al dente, then toss it with garlic and olive oil.",
        ),
        (
            "rates",
            "The central bank raised interest rates to slow inflation.",
        ),
        (
            "hiking",
            "The trail climbs through pine forest to a ridge above the lake.",
        ),
        (
            "rust",
            "Ownership and borrowing let Rust guarantee memory safety without a garbage collector.",
        ),
    ];
    for (id, content) in articles {
        db.batch(&[BatchOperation::JsonPut {
            space: "articles".into(),
            id: id.into(),
            value: json!({ "content": content }),
            if_version: None,
        }])?;
    }

    // Inserts return immediately; embedding happens in the background.
    db.wait_for_embeddings(Duration::from_secs(60))?;

    for query in [
        "how do I cook noodles?",
        "monetary policy",
        "safe systems programming",
    ] {
        let hits = db.vector_search_text("articles", query, 2)?;
        println!("{query}");
        for hit in hits {
            println!("  {:<8} distance {:.3}", hit.id, hit.distance);
        }
    }

    db.close(Duration::from_secs(5))?;
    Ok(())
}
