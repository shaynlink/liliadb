//! Full-text search: indexing, BM25 ranking, query syntax, snippets, suggestions, fuzzy and
//! phonetic search, security, transactions, ledger.

mod common;

use common::small_options;
use lilia_db::{Batch, Database, FtsWrite, LedgerVerifyMode, Policy, SecurityContext, Write};
use lilia_engine::Durability;
use serde_json::json;

fn open(dir: &std::path::Path, ledger: bool) -> Database {
    Database::open(dir, small_options(Durability::Performance, ledger)).unwrap()
}

fn ids(hits: &[lilia_db::FtsHit]) -> Vec<&str> {
    hits.iter().map(|h| h.doc_id.as_str()).collect()
}

fn sorted(hits: &[lilia_db::FtsHit]) -> Vec<&str> {
    let mut ids = ids(hits);
    ids.sort_unstable();
    ids
}

fn corpus(db: &Database) {
    let s = db.system();
    let f = s.fts("articles");
    f.index("a1", "The quick brown fox jumps over the lazy dog")
        .unwrap();
    f.index("a2", "A fox, a fox, and another fox: foxes everywhere")
        .unwrap();
    f.index("a3", "Brown bears and brown dogs share the forest")
        .unwrap();
    f.index("a4", "Quantum computing explained for the curious mind")
        .unwrap();
    f.index("a5", "The lazy afternoon of a sleepy dog").unwrap();
}

#[test]
fn search_ranks_with_bm25_and_follows_the_query_syntax() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    corpus(&db);
    let f = db.system();
    let f = f.fts("articles");

    let fox = f.search("fox", 10).unwrap();
    assert_eq!(ids(&fox), ["a2", "a1"], "three occurrences rank above one");
    assert!(
        fox[0].rank < fox[1].rank && fox[1].rank < 0.0,
        "rank is the negated score"
    );
    assert_eq!(ids(&f.search("lazy dog", 10).unwrap()).len(), 2);
    assert_eq!(
        ids(&f.search("\"lazy dog\"", 10).unwrap()),
        ["a1"],
        "a phrase needs consecutive words"
    );
    assert_eq!(
        ids(&f.search("\"dog lazy\"", 10).unwrap()),
        Vec::<&str>::new()
    );
    assert_eq!(
        sorted(&f.search("quantum OR bears", 10).unwrap()),
        ["a3", "a4"]
    );
    assert_eq!(
        ids(&f.search("dog NOT lazy", 10).unwrap()),
        Vec::<&str>::new(),
        "a3 says dogs, not dog"
    );
    assert_eq!(ids(&f.search("brown NOT fox", 10).unwrap()), ["a3"]);
    assert_eq!(sorted(&f.search("fox*", 10).unwrap()), ["a1", "a2"]);
    assert_eq!(
        ids(&f.search("(quick OR sleepy) AND dog", 10).unwrap()).len(),
        2
    );
    assert_eq!(f.search("fox", 1).unwrap().len(), 1);
    assert!(f.search("   ", 10).unwrap().is_empty());
    assert!(f.search("absent", 10).unwrap().is_empty());
    assert!(db
        .system()
        .fts("empty")
        .search("fox", 10)
        .unwrap()
        .is_empty());

    let hit = &f.search("quantum", 1).unwrap()[0];
    assert_eq!(
        hit.snippet,
        "<b>Quantum</b> computing explained for the curious mind"
    );
}

#[test]
fn reindexing_and_deleting_keep_the_index_exact() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    corpus(&db);
    let s = db.system();
    let f = s.fts("articles");
    f.index("a4", "A fox in a quantum box").unwrap();
    assert_eq!(
        ids(&f.search("computing", 10).unwrap()),
        Vec::<&str>::new(),
        "the old words are gone"
    );
    assert!(ids(&f.search("fox", 10).unwrap()).contains(&"a4"));
    assert!(f.delete("a2").unwrap());
    assert!(!f.delete("a2").unwrap());
    assert!(!ids(&f.search("fox", 10).unwrap()).contains(&"a2"));
    f.index("a1", "  ,,, ").unwrap();
    assert!(
        f.search("quick", 10).unwrap().is_empty(),
        "content without words removes the document"
    );
    f.index("enc", "visible ENC:secret").unwrap();
    assert_eq!(ids(&f.search("visible", 10).unwrap()), ["enc"]);
    assert!(
        f.search("secret", 10).unwrap().is_empty(),
        "encrypted words are not indexed"
    );
    db.close().unwrap();
    let db = open(dir.path(), false);
    assert_eq!(
        ids(&db.system().fts("articles").search("box", 10).unwrap()),
        ["a4"]
    );
}

#[test]
fn suggestions_fuzzy_and_phonetic_search_use_the_vocabulary() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    corpus(&db);
    let s = db.system();
    let f = s.fts("articles");
    let suggested: Vec<String> = f
        .suggest("fo", 1, 5)
        .unwrap()
        .into_iter()
        .map(|s| s.term)
        .collect();
    assert_eq!(&suggested[..3], ["fox", "for", "foxes"]);
    assert_eq!(
        f.suggest("fox", 0, 5)
            .unwrap()
            .iter()
            .find(|s| s.term == "fox")
            .unwrap()
            .doc_count,
        2
    );

    f.load_dictionary(&["Foxglove"]).unwrap();
    assert!(
        f.suggest("foxg", 0, 5)
            .unwrap()
            .iter()
            .any(|s| s.term == "foxglove"),
        "dictionary words are suggested"
    );
    f.index("a6", "foxtrot dancing").unwrap();
    assert!(
        f.suggest("foxt", 0, 5)
            .unwrap()
            .iter()
            .any(|s| s.term == "foxtrot"),
        "a loaded vocabulary follows new documents"
    );
    f.delete("a6").unwrap();
    assert!(
        !f.suggest("foxt", 0, 5)
            .unwrap()
            .iter()
            .any(|s| s.term == "foxtrot"),
        "and deletions"
    );

    let fuzzy = f.fuzzy_search("quik bown", 2, 10).unwrap();
    assert_eq!(
        fuzzy.iter().map(|h| h.doc_id.as_str()).collect::<Vec<_>>(),
        ["a1"]
    );
    assert_eq!(fuzzy[0].edit_distance, 1);
    assert!(fuzzy[0].matched_terms.contains(&"quick".to_owned()));
    let exact = f.fuzzy_search("fox", 2, 10).unwrap();
    assert!(exact.iter().all(|h| h.edit_distance == 0));

    db.system()
        .fts("people")
        .index("p1", "Contact Smyth about the invoice")
        .unwrap();
    db.system()
        .fts("people")
        .index("p2", "Robert Smith signed it")
        .unwrap();
    let phonetic = db
        .system()
        .fts("people")
        .phonetic_search("Smith", 10)
        .unwrap();
    let mut found: Vec<&str> = phonetic.iter().map(|h| h.doc_id.as_str()).collect();
    found.sort_unstable();
    assert_eq!(found, ["p1", "p2"]);
    assert_eq!(phonetic[0].doc_id, "p2", "the exact spelling ranks first");
    assert_ne!(phonetic[0].phonetic_codes, [] as [std::string::String; 0]);
}

#[test]
fn hits_pass_the_callers_read_policies() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    let s = db.system();
    s.json_put("notes", "n1", &json!({"tenant": "a"})).unwrap();
    s.json_put("notes", "n2", &json!({"tenant": "b"})).unwrap();
    s.fts("notes").index("n1", "shared keyword alpha").unwrap();
    s.fts("notes").index("n2", "shared keyword beta").unwrap();
    s.fts("notes").index("n3", "shared keyword orphan").unwrap();
    db.create_policy(
        "tenant",
        Policy::new("notes", "$.tenant == current_context('tenant')"),
    )
    .unwrap();
    let a = db.session(SecurityContext::from_pair("tenant", "a"));
    assert_eq!(
        ids(&a.fts("notes").search("shared", 10).unwrap()),
        ["n1"],
        "n2 is another tenant's, n3 has no tenant"
    );
    assert_eq!(s.fts("notes").search("shared", 10).unwrap().len(), 3);
}

#[test]
fn transactions_search_their_own_documents() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    corpus(&db);
    let s = db.system();
    let tx = s.transaction();
    tx.fts("articles")
        .index("t1", "a fox in the transaction")
        .unwrap();
    assert!(tx.fts("articles").delete("a2").unwrap());
    let inside = tx.fts("articles").search("fox", 10).unwrap();
    assert_eq!(sorted(&inside), ["a1", "t1"]);
    assert_eq!(
        ids(&s.fts("articles").search("fox", 10).unwrap()),
        ["a2", "a1"],
        "nothing outside before commit"
    );
    tx.commit().unwrap();
    assert_eq!(
        sorted(&s.fts("articles").search("fox", 10).unwrap()),
        ["a1", "t1"]
    );
}

#[test]
fn indexed_documents_and_dictionary_words_are_ledger_records() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), true);
    corpus(&db);
    let f = db.system();
    let f = f.fts("articles");
    f.load_dictionary(&["zebra"]).unwrap();
    f.delete("a5").unwrap();
    let report = db.verify_ledger(LedgerVerifyMode::Strict).unwrap();
    assert!(report.is_valid, "{report:?}");
    assert_eq!((report.commit_count, report.leaves_verified), (7, 5));
    assert!(db
        .prove_fts_doc("articles", "a1")
        .unwrap()
        .verify(&report.merkle_root));
}

/// Concurrent writers re-index the same documents: the index ends exactly as the last content
/// of each document says.
#[test]
fn concurrent_indexers_leave_an_exact_index() {
    let dir = tempfile::tempdir().unwrap();
    let db = std::sync::Arc::new(open(dir.path(), false));
    let writers: Vec<_> = (0..8)
        .map(|t| {
            let db = std::sync::Arc::clone(&db);
            std::thread::spawn(move || {
                for i in 0..40 {
                    let doc = format!("d{}", i % 20);
                    db.system()
                        .fts("c")
                        .index(&doc, &format!("common w{t} round{i}"))
                        .unwrap();
                }
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }
    let s = db.system();
    let f = s.fts("c");
    assert_eq!(
        f.search("common", 100).unwrap().len(),
        20,
        "every document once"
    );
    let per_writer: usize = (0..8)
        .map(|t| f.search(&format!("w{t}"), 100).unwrap().len())
        .sum();
    assert_eq!(
        per_writer, 20,
        "each document holds only its last writer's words"
    );
    let rounds: usize = (0..40)
        .map(|i| f.search(&format!("round{i}"), 100).unwrap().len())
        .sum();
    assert_eq!(rounds, 20);
    assert_eq!(f.suggest("common", 0, 5).unwrap()[0].doc_count, 20);
}

#[test]
fn a_batch_indexes_like_single_writes() {
    // Documents of one batch share the collection's count shards and, here, their terms: each
    // write must see the counts the earlier ones left.
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), false);
    let s = db.system();
    let texts: Vec<String> = (0..200)
        .map(|i| format!("sensor reading {} value {}", i % 7, i % 13))
        .collect();
    let mut batch = Batch::new();
    for (i, text) in texts.iter().enumerate() {
        batch.push(Write::Fts(FtsWrite::Index {
            collection: "batched".into(),
            doc_id: format!("d{i:03}"),
            content: text.clone(),
        }));
    }
    s.commit(&batch).unwrap();
    for (i, text) in texts.iter().enumerate() {
        s.fts("single").index(&format!("d{i:03}"), text).unwrap();
    }
    let ranks = |name: &str| -> Vec<(String, f64)> {
        s.fts(name)
            .search("reading 3 value", 50)
            .unwrap()
            .into_iter()
            .map(|h| (h.doc_id, h.rank))
            .collect()
    };
    let (batched, single) = (ranks("batched"), ranks("single"));
    assert_ne!(single.len(), 0);
    assert_eq!(batched.len(), single.len());
    for ((a, x), (b, y)) in batched.iter().zip(&single) {
        assert_eq!(a, b);
        assert!((x - y).abs() < 1e-9, "{a}: {x} != {y}");
    }
}
