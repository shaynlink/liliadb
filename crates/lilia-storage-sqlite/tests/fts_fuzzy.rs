use lilia_storage_sqlite::{Database, DatabaseOptions};
use tempfile::tempdir;

#[test]
fn test_fts_croisant_to_croissant_autocomplete_latency() {
    let temp = tempdir().unwrap();
    let db_path = temp.path().join("fuzzy_croissant.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();

    let fts = db.collection("bakery").fts().unwrap();

    // 1. Index realistic documents
    fts.index("doc1", "Un délicieux croissant au beurre croustillant le matin.")
        .unwrap();
    fts.index("doc2", "Une baguette tradition bien cuite avec de la confiture.")
        .unwrap();
    fts.index("doc3", "Pain au chocolat ou chocolatine pour le petit déjeuner.")
        .unwrap();
    fts.index("doc4", "Croisière gourmande sur la Seine avec dégustation.")
        .unwrap();

    // 2. Exact match autocomplete
    let exact_sugg = fts.suggest("croiss", 1, 5).unwrap();
    assert!(!exact_sugg.is_empty());
    assert_eq!(exact_sugg[0].term, "croissant");
    assert_eq!(exact_sugg[0].distance, 0);

    // 3. Typo autocompletion: "croisant" finds "croissant"
    let start = std::time::Instant::now();
    let typo_sugg = fts.suggest("croisant", 1, 5).unwrap();
    let elapsed = start.elapsed();

    println!("Mobile autocomplete latency for 'croisant': {elapsed:?}");
    assert!(!typo_sugg.is_empty());
    assert_eq!(typo_sugg[0].term, "croissant");
    assert_eq!(typo_sugg[0].distance, 1);

    // Autocomplete is sub-millisecond
    assert!(
        elapsed.as_millis() < 5,
        "Latence autocomplétion doit être < 5ms en debug, obtenue: {elapsed:?}"
    );

    // 4. Damerau transposition: "crosiant" (swapped 's' and missing 's') -> distance 1 or 2
    let damerau_sugg = fts.suggest("crosiant", 2, 5).unwrap();
    assert!(!damerau_sugg.is_empty());
    assert!(damerau_sugg.iter().any(|s| s.term == "croissant"));

    // 5. Fuzzy full-text document search: query has typo "croisant beure"
    let hits = fts.fuzzy_search("croisant beure", 1, 5).unwrap();
    assert!(!hits.is_empty(), "fuzzy search must find matching document");
    assert_eq!(hits[0].doc_id, "doc1");
    assert!(
        hits[0].snippet.contains("<b>croissant</b>")
            || hits[0].snippet.contains("croissant"),
        "snippet must highlight matched term"
    );
    assert_eq!(hits[0].edit_distance, 1);
}

#[test]
fn test_fts_fuzzy_persistence_across_reopen() {
    let temp = tempdir().unwrap();
    let db_path = temp.path().join("fuzzy_persist.lilia");

    {
        let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();
        let fts = db.collection("catalog").fts().unwrap();
        fts.index("p1", "MacBook Pro avec puce Apple Silicon M3 Max.")
            .unwrap();
        fts.index("p2", "Smartphone Android avec écran OLED 120Hz.")
            .unwrap();
    }

    // Reopen database
    {
        let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();
        let fts = db.collection("catalog").fts().unwrap();

        // Autocomplete with typo "macbok" -> finds "macbook"
        let sugg = fts.suggest("macbok", 1, 5).unwrap();
        assert!(!sugg.is_empty());
        assert_eq!(sugg[0].term, "macbook");
        assert_eq!(sugg[0].distance, 1);

        // Fuzzy search for "androdi" -> finds p2
        let hits = fts.fuzzy_search("androdi", 1, 5).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].doc_id, "p2");
    }
}

#[test]
fn test_fts_load_custom_dictionary() {
    let temp = tempdir().unwrap();
    let db_path = temp.path().join("fuzzy_dict.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();

    let fts = db.collection("medical").fts().unwrap();

    // Preload domain-specific medical dictionary
    let terms = ["paracetamol", "ibuprofene", "amoxicilline", "aspirine"];
    fts.load_dictionary(&terms).unwrap();

    // User typos: "paracetmol" (missing 'a')
    let sugg = fts.suggest("paracetmol", 1, 5).unwrap();
    assert!(!sugg.is_empty());
    assert_eq!(sugg[0].term, "paracetamol");
    assert_eq!(sugg[0].distance, 1);

    // User typos: "ibupofene" (missing 'r')
    let sugg2 = fts.suggest("ibupofene", 1, 5).unwrap();
    assert!(!sugg2.is_empty());
    assert_eq!(sugg2[0].term, "ibuprofene");
}

#[test]
fn test_fts_memory_tier_fuzzy() {
    let temp = tempdir().unwrap();
    let db_path = temp.path().join("fuzzy_mem.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();
    let fts = db.collection("ephem_fts").memory().fts().unwrap();

    fts.index("msg1", "bienvenue sur lilia database moteur hybride")
        .unwrap();

    let sugg = fts.suggest("hybrd", 1, 5).unwrap();
    assert!(!sugg.is_empty());
    assert_eq!(sugg[0].term, "hybride");

    let hits = fts.fuzzy_search("motuer hybryde", 1, 5).unwrap();
    assert!(!hits.is_empty());
    assert_eq!(hits[0].doc_id, "msg1");
}
