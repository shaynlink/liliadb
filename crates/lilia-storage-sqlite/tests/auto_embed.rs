use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use lilia_core::rules::{Action, Rule};
use lilia_core::{
    BatchOperation, Embedder, ErrorCode, FieldCipherAlgorithm, HnswConfig, LiliaError,
    VectorElementType,
};
use lilia_storage_sqlite::{
    AutoEmbedConfig, BuiltinModel, CollectionKind, CollectionOptions, CollectionSpec, Database,
    DatabaseOptions, EmbedOptions, StorageTier,
};
use serde_json::{json, Value};

const MODEL: BuiltinModel = BuiltinModel::MiniLmL6V2;
const WAIT: Duration = Duration::from_secs(20);

/// Deterministic stand-in for a real model: a normalised bag of hashed words, so texts sharing
/// words are close. Texts containing `poison` fail, and each call may be slowed down.
#[derive(Debug, Default)]
struct BagOfWords {
    embedded_texts: AtomicUsize,
    delay: Duration,
}

impl BagOfWords {
    fn slow(delay: Duration) -> Self {
        Self {
            embedded_texts: AtomicUsize::new(0),
            delay,
        }
    }

    fn embedded(&self) -> usize {
        self.embedded_texts.load(Ordering::SeqCst)
    }

    fn vector(text: &str) -> Vec<f32> {
        let dimension = MODEL.dimension() as usize;
        let mut vector = vec![0.0_f32; dimension];
        for word in text.to_lowercase().split_whitespace() {
            let slot = word.bytes().fold(7_usize, |hash, byte| {
                hash.wrapping_mul(31) + usize::from(byte)
            });
            vector[slot % dimension] += 1.0;
        }
        let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for component in &mut vector {
                *component /= norm;
            }
        }
        vector
    }
}

impl Embedder for BagOfWords {
    fn model(&self) -> BuiltinModel {
        MODEL
    }

    fn embed(&self, texts: &[&str]) -> lilia_core::Result<Vec<Vec<f32>>> {
        std::thread::sleep(self.delay);
        if let Some(text) = texts.iter().find(|text| text.contains("poison")) {
            return Err(LiliaError::new(
                ErrorCode::Storage,
                format!("cannot embed '{text}'"),
                false,
            ));
        }
        self.embedded_texts.fetch_add(texts.len(), Ordering::SeqCst);
        Ok(texts.iter().map(|text| Self::vector(text)).collect())
    }
}

fn bytes(vector: &[f32]) -> Vec<u8> {
    vector.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn options(directory: &tempfile::TempDir) -> DatabaseOptions {
    DatabaseOptions::durable(directory.path().join("app.lilia")).with_embed_options(EmbedOptions {
        batch_size: 8,
        max_attempts: 3,
        retry_backoff_ms: 5,
    })
}

fn open_with(directory: &tempfile::TempDir, embedder: &Arc<BagOfWords>) -> Database {
    let embedder: Arc<dyn Embedder> = embedder.clone();
    Database::open(options(directory).with_embedder(embedder)).unwrap()
}

fn open() -> (tempfile::TempDir, Arc<BagOfWords>, Database) {
    let directory = tempfile::tempdir().unwrap();
    let embedder = Arc::new(BagOfWords::default());
    let database = open_with(&directory, &embedder);
    (directory, embedder, database)
}

fn embed_content() -> CollectionOptions {
    CollectionOptions {
        auto_embed: Some(AutoEmbedConfig::new("content", MODEL)),
        ..CollectionOptions::default()
    }
}

fn articles(database: &Database) {
    database
        .json()
        .create_collection("articles", embed_content())
        .unwrap();
}

fn put(space: &str, id: &str, value: Value) -> BatchOperation {
    BatchOperation::JsonPut {
        space: space.into(),
        id: id.into(),
        value,
        if_version: None,
    }
}

fn write(database: &Database, id: &str, content: &str) {
    database
        .batch(&[put("articles", id, json!({ "content": content }))])
        .unwrap();
}

fn stored_vector(database: &Database, id: &str) -> Option<Vec<u8>> {
    database
        .vector_get("articles", id)
        .unwrap()
        .map(|(dim, element_type, vector)| {
            assert_eq!(dim, MODEL.dimension());
            assert_eq!(element_type, VectorElementType::Float32);
            vector
        })
}

fn code(result: lilia_core::Result<()>) -> ErrorCode {
    result.unwrap_err().code
}

#[test]
fn inserted_documents_become_searchable_by_text() {
    let (_directory, _embedder, database) = open();
    articles(&database);
    write(&database, "cats", "kittens purr and chase mice");
    write(&database, "rates", "central banks raise interest rates");
    write(&database, "bread", "sourdough needs flour water salt");

    database.wait_for_embeddings(WAIT).unwrap();

    assert_eq!(
        stored_vector(&database, "cats"),
        Some(bytes(&BagOfWords::vector("kittens purr and chase mice")))
    );
    let index = database.vector_get_index_info("articles").unwrap().unwrap();
    assert_eq!((index.dim, index.node_count), (MODEL.dimension(), 3));

    let hits = database
        .vector_search_text("articles", "interest rates banks", 2)
        .unwrap();
    assert_eq!(hits[0].id, "rates");
    assert_eq!(hits.len(), 2);
}

#[test]
fn embedding_never_rewrites_the_document() {
    let (_directory, _embedder, database) = open();
    articles(&database);
    write(&database, "a", "first draft");
    database.wait_for_embeddings(WAIT).unwrap();

    assert_eq!(
        database.json_get("articles", "a").unwrap().unwrap().version,
        1
    );
    database
        .batch(&[BatchOperation::JsonPut {
            space: "articles".into(),
            id: "a".into(),
            value: json!({ "content": "second draft" }),
            if_version: Some(1),
        }])
        .unwrap();
}

#[test]
fn only_a_changed_source_text_is_embedded_again() {
    let (_directory, embedder, database) = open();
    articles(&database);
    write(&database, "a", "kittens purr");
    database.wait_for_embeddings(WAIT).unwrap();
    assert_eq!(embedder.embedded(), 1);

    database
        .batch(&[put(
            "articles",
            "a",
            json!({ "content": "kittens purr", "views": 12 }),
        )])
        .unwrap();
    assert_eq!(database.json().embed_status("articles").unwrap().pending, 0);
    database.wait_for_embeddings(WAIT).unwrap();
    assert_eq!(embedder.embedded(), 1);

    write(&database, "a", "puppies bark");
    database.wait_for_embeddings(WAIT).unwrap();
    assert_eq!(embedder.embedded(), 2);
    assert_eq!(
        stored_vector(&database, "a"),
        Some(bytes(&BagOfWords::vector("puppies bark")))
    );
}

#[test]
fn a_document_without_text_loses_its_vector() {
    let (_directory, _embedder, database) = open();
    articles(&database);
    for id in ["deleted", "blanked", "retyped", "kept"] {
        write(&database, id, "kittens purr");
    }
    database.wait_for_embeddings(WAIT).unwrap();

    database
        .batch(&[
            BatchOperation::JsonDelete {
                space: "articles".into(),
                id: "deleted".into(),
                if_version: None,
            },
            put("articles", "blanked", json!({ "title": "no content" })),
            put("articles", "retyped", json!({ "content": 42 })),
        ])
        .unwrap();
    database.wait_for_embeddings(WAIT).unwrap();

    for id in ["deleted", "blanked", "retyped"] {
        assert_eq!(stored_vector(&database, id), None, "{id}");
    }
    assert!(stored_vector(&database, "kept").is_some());
    let hits = database
        .vector_search_text("articles", "kittens purr", 10)
        .unwrap();
    assert_eq!(
        hits.iter().map(|hit| hit.id.as_str()).collect::<Vec<_>>(),
        ["kept"]
    );
}

#[test]
fn queued_documents_survive_a_restart_without_embedder() {
    let directory = tempfile::tempdir().unwrap();
    let embedder = Arc::new(BagOfWords::default());
    {
        let database = open_with(&directory, &embedder);
        articles(&database);
        database.close(WAIT).unwrap();
    }
    {
        let database = Database::open(options(&directory)).unwrap();
        write(&database, "a", "kittens purr");
        assert_eq!(database.json().embed_status("articles").unwrap().pending, 1);
        assert_eq!(
            code(database.wait_for_embeddings(Duration::from_millis(50))),
            ErrorCode::Unsupported
        );
        database.close(WAIT).unwrap();
    }

    let database = open_with(&directory, &embedder);
    database.wait_for_embeddings(WAIT).unwrap();
    assert!(stored_vector(&database, "a").is_some());
}

#[test]
fn enabling_on_an_existing_collection_embeds_what_it_holds() {
    let (_directory, _embedder, database) = open();
    for id in ["a", "b", "c"] {
        write(&database, id, "kittens purr");
    }
    assert_eq!(stored_vector(&database, "a"), None);

    let config = AutoEmbedConfig::new("content", MODEL);
    database
        .json()
        .set_auto_embed("articles", Some(&config))
        .unwrap();
    database.wait_for_embeddings(WAIT).unwrap();
    for id in ["a", "b", "c"] {
        assert!(stored_vector(&database, id).is_some(), "{id}");
    }

    database.json().set_auto_embed("articles", None).unwrap();
    write(&database, "d", "puppies bark");
    database.wait_for_embeddings(WAIT).unwrap();
    assert_eq!(stored_vector(&database, "d"), None);
    assert!(stored_vector(&database, "a").is_some());
    assert_eq!(
        database
            .vector_search_text("articles", "kittens", 1)
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
}

#[test]
fn reembed_recomputes_every_vector() {
    let (_directory, embedder, database) = open();
    articles(&database);
    write(&database, "a", "kittens purr");
    write(&database, "b", "puppies bark");
    database.wait_for_embeddings(WAIT).unwrap();
    assert!(database.vector_delete("articles", "a").unwrap());

    database.json().reembed("articles").unwrap();
    database.wait_for_embeddings(WAIT).unwrap();

    assert!(stored_vector(&database, "a").is_some());
    assert_eq!(embedder.embedded(), 4);
}

#[test]
fn declarations_that_cannot_work_are_refused() {
    let directory = tempfile::tempdir().unwrap();
    {
        let database = Database::open(options(&directory)).unwrap();
        assert_eq!(
            code(
                database
                    .json()
                    .create_collection("articles", embed_content())
            ),
            ErrorCode::Unsupported
        );
        database.close(WAIT).unwrap();
    }

    let embedder = Arc::new(BagOfWords::default());
    let database = open_with(&directory, &embedder);
    let json = database.json();

    let blank = CollectionOptions {
        auto_embed: Some(AutoEmbedConfig::new("  ", MODEL)),
        ..CollectionOptions::default()
    };
    assert_eq!(
        code(json.create_collection("blank", blank)),
        ErrorCode::InvalidInput
    );

    database.collection("volatile").memory().json().unwrap();
    let config = AutoEmbedConfig::new("content", MODEL);
    assert_eq!(
        code(json.set_auto_embed("volatile", Some(&config))),
        ErrorCode::Unsupported
    );

    database
        .collection("secrets")
        .field_encryption("content", FieldCipherAlgorithm::RandomizedGcm, "vault")
        .json()
        .unwrap();
    assert_eq!(
        code(json.set_auto_embed("secrets", Some(&config))),
        ErrorCode::InvalidInput
    );

    database
        .vector_set(
            "small",
            "v",
            3,
            VectorElementType::Float32,
            &bytes(&[1.0, 0.0, 0.0]),
        )
        .unwrap();
    assert_eq!(
        code(json.create_collection("small", embed_content())),
        ErrorCode::InvalidInput
    );
    database
        .vector_create_index(
            "indexed",
            HnswConfig {
                dim: 3,
                ..HnswConfig::default()
            },
        )
        .unwrap();
    assert_eq!(
        code(json.create_collection("indexed", embed_content())),
        ErrorCode::InvalidInput
    );

    // A refused declaration leaves nothing behind: the same name can be declared afterwards.
    json.create_collection("blank", embed_content()).unwrap();
}

#[test]
fn an_auto_embedded_collection_stays_on_disk_and_readable() {
    let (_directory, _embedder, database) = open();
    articles(&database);

    assert_eq!(
        database
            .collection("articles")
            .memory()
            .json()
            .unwrap_err()
            .code,
        ErrorCode::Unsupported
    );
    assert_eq!(
        database
            .collection("articles")
            .memory()
            .configure(CollectionKind::Vector)
            .unwrap_err()
            .code,
        ErrorCode::Unsupported
    );
    assert_eq!(
        database
            .collection("articles")
            .field_encryption("content", FieldCipherAlgorithm::RandomizedGcm, "vault")
            .json()
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
}

#[test]
fn failing_documents_are_reported_then_rearmed() {
    let (_directory, _embedder, database) = open();
    articles(&database);
    database
        .batch(&[
            put("articles", "good", json!({ "content": "kittens purr" })),
            put("articles", "bad", json!({ "content": "poison pill" })),
        ])
        .unwrap();

    database.wait_for_embeddings(WAIT).unwrap();
    let status = database.json().embed_status("articles").unwrap();
    assert_eq!((status.pending, status.failed), (0, 1));
    assert!(status.last_error.unwrap().contains("poison pill"));
    assert!(stored_vector(&database, "good").is_some());
    assert_eq!(stored_vector(&database, "bad"), None);

    write(&database, "bad", "harmless text");
    database.wait_for_embeddings(WAIT).unwrap();
    let status = database.json().embed_status("articles").unwrap();
    assert_eq!(
        (status.pending, status.failed, status.last_error),
        (0, 0, None)
    );
    assert!(stored_vector(&database, "bad").is_some());
}

#[test]
fn documents_written_by_rules_are_embedded() {
    let (_directory, _embedder, database) = open();
    articles(&database);
    database
        .rules()
        .register(
            "summarise_alerts",
            Rule {
                when: "events.level == 'critical'".to_string(),
                window: None,
                then: Action::Insert {
                    collection: "articles".to_string(),
                    document: json!({ "content": "critical alert raised" }),
                },
                target: None,
            },
        )
        .unwrap();

    database
        .batch(&[put("events", "e1", json!({ "level": "critical" }))])
        .unwrap();
    database.wait_for_embeddings(WAIT).unwrap();

    let hits = database
        .vector_search_text("articles", "critical alert", 1)
        .unwrap();
    assert_eq!(hits.len(), 1);
}

#[test]
fn undo_and_redo_keep_the_vector_in_step_with_the_document() {
    let (_directory, _embedder, database) = open();
    articles(&database);
    write(&database, "a", "kittens purr");
    database.wait_for_embeddings(WAIT).unwrap();
    write(&database, "a", "puppies bark");
    database.wait_for_embeddings(WAIT).unwrap();

    database.undo().unwrap().unwrap();
    database.wait_for_embeddings(WAIT).unwrap();
    assert_eq!(
        stored_vector(&database, "a"),
        Some(bytes(&BagOfWords::vector("kittens purr")))
    );

    database.redo().unwrap().unwrap();
    database.wait_for_embeddings(WAIT).unwrap();
    assert_eq!(
        stored_vector(&database, "a"),
        Some(bytes(&BagOfWords::vector("puppies bark")))
    );
}

#[test]
fn closing_while_embedding_loses_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let slow = Arc::new(BagOfWords::slow(Duration::from_millis(15)));
    {
        let database = open_with(&directory, &slow);
        articles(&database);
        for index in 0..40 {
            write(&database, &format!("doc{index}"), "kittens purr");
        }
        database.close(WAIT).unwrap();
    }

    let embedder = Arc::new(BagOfWords::default());
    let database = open_with(&directory, &embedder);
    database.wait_for_embeddings(WAIT).unwrap();
    for index in 0..40 {
        assert!(
            stored_vector(&database, &format!("doc{index}")).is_some(),
            "doc{index}"
        );
    }
}

#[test]
fn explicit_collection_specs_do_not_drop_auto_embed() {
    let directory = tempfile::tempdir().unwrap();
    let embedder = Arc::new(BagOfWords::default());
    {
        let database = open_with(&directory, &embedder);
        articles(&database);
        database.close(WAIT).unwrap();
    }

    let backend: Arc<dyn Embedder> = embedder.clone();
    let database = Database::open(
        options(&directory)
            .with_embedder(backend)
            .with_collection_spec(CollectionSpec::new(
                CollectionKind::Json,
                "articles",
                StorageTier::Disk,
            )),
    )
    .unwrap();
    write(&database, "a", "kittens purr");
    database.wait_for_embeddings(WAIT).unwrap();
    assert!(stored_vector(&database, "a").is_some());
}

#[test]
fn text_search_needs_an_auto_embedded_collection() {
    let (_directory, _embedder, database) = open();
    assert_eq!(
        database
            .vector_search_text("articles", "kittens", 1)
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
    assert_eq!(
        database.json().reembed("articles").unwrap_err().code,
        ErrorCode::InvalidInput
    );
    database.wait_for_embeddings(Duration::ZERO).unwrap();
}
