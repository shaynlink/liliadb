//! Semantic search: the embedder turns texts into vectors, `lilia-db` stores and searches them.
//!
//! `cargo run -p lilia-embed --example semantic_search -- <folder with the all-MiniLM-L6-v2 files>`

use lilia_db::vectors::Vector;
use lilia_db::{Database, Options, VectorMetric};
use lilia_embed::{BuiltinModel, CandleEmbedder, DevicePreference};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model_dir = std::env::args()
        .nth(1)
        .ok_or("usage: semantic_search <folder with the all-MiniLM-L6-v2 files>")?;
    let embedder =
        CandleEmbedder::load(BuiltinModel::MiniLmL6V2, model_dir, DevicePreference::Auto)?;

    let directory = tempfile::tempdir()?;
    let db = Database::open(directory.path().join("articles"), Options::default())?;
    let session = db.system();
    let articles = session.vectors("articles");

    let texts = [
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
    let vectors = embedder.embed_texts(&texts.map(|(_, text)| text))?;
    for ((id, _), vector) in texts.iter().zip(&vectors) {
        articles.set(id, &Vector::from_f32(vector)?)?;
    }

    for query in [
        "how do I cook noodles?",
        "monetary policy",
        "safe systems programming",
    ] {
        let vector = embedder.embed_texts(&[query])?.remove(0);
        println!("{query}");
        for hit in articles.search(&Vector::from_f32(&vector)?, VectorMetric::Cosine, 2)? {
            println!("  {:<8} distance {:.3}", hit.id, hit.distance);
        }
    }
    drop(session);
    db.close()?;
    Ok(())
}
