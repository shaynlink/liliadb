use lilia_storage_sqlite::{Database, DatabaseOptions};
use tempfile::tempdir;

#[test]
fn test_fts_phonetic_exact_matches() {
    let temp = tempdir().unwrap();
    let db_path = temp.path().join("phonetic_test.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();

    let fts = db.collection("medical_directory").fts().unwrap();

    // Index documents with varying names and brands
    fts.index(
        "doc1",
        "Dr. Guerard Smith is the lead researcher at Fizer pharmaceuticals.",
    )
    .unwrap();
    fts.index(
        "doc2",
        "Dr. Alexandre Dupont works at Novartis oncology unit in Paris.",
    )
    .unwrap();
    fts.index(
        "doc3",
        "Katherine Smyth published a study on ibuprofen and paracetamol.",
    )
    .unwrap();

    // 1. "Gérard" finds "Guerard"
    let hits_gerard = fts.phonetic_search("Gérard", 5).unwrap();
    assert!(!hits_gerard.is_empty(), "Gérard must find Guerard");
    assert_eq!(hits_gerard[0].doc_id, "doc1");
    assert!(
        hits_gerard[0].snippet.contains("Guerard"),
        "Snippet must contain Guerard"
    );
    assert!(hits_gerard[0]
        .matched_terms
        .contains(&"guerard".to_string()));

    // 2. "Smith" finds "Smyth"
    let hits_smith = fts.phonetic_search("Smith", 5).unwrap();
    assert!(!hits_smith.is_empty(), "Smith must find Smyth or Smith");
    let doc_ids: Vec<&str> = hits_smith.iter().map(|h| h.doc_id.as_str()).collect();
    assert!(doc_ids.contains(&"doc1") || doc_ids.contains(&"doc3"));

    // 3. "Pfizer" finds "Fizer"
    let hits_pfizer = fts.phonetic_search("Pfizer", 5).unwrap();
    assert!(!hits_pfizer.is_empty(), "Pfizer must find Fizer");
    assert_eq!(hits_pfizer[0].doc_id, "doc1");
    assert!(
        hits_pfizer[0].snippet.contains("Fizer"),
        "Snippet must contain Fizer"
    );

    // 4. Multi-word phonetic query: "Gérard Smyth Pfizer" finds doc1
    let hits_multi = fts.phonetic_search("Gérard Smyth Pfizer", 5).unwrap();
    assert!(!hits_multi.is_empty(), "Gérard Smyth Pfizer must find doc1");
    assert_eq!(hits_multi[0].doc_id, "doc1");
    assert!(hits_multi[0].snippet.contains("Guerard"));
    assert!(hits_multi[0].snippet.contains("Smith"));
    assert!(hits_multi[0].snippet.contains("Fizer"));
}

#[test]
fn test_fts_cjk_chinese_japanese_korean() {
    let temp = tempdir().unwrap();
    let db_path = temp.path().join("cjk_test.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();

    let fts = db.collection("asian_knowledge").fts().unwrap();

    // 1. Index Chinese text (unspaced)
    fts.index(
        "cn_doc",
        "LiliaDB 是一个高性能分布式多模型数据库，支持毫秒级全文检索。",
    )
    .unwrap();

    // 2. Index Japanese text (unspaced)
    fts.index(
        "jp_doc",
        "東京特許許可局で日本語の全文検索エンジンをテストします。",
    )
    .unwrap();

    // 3. Index Korean text
    fts.index(
        "ko_doc",
        "릴리아 데이터베이스에서 한국어 전문 검색 엔진을 테스트합니다.",
    )
    .unwrap();

    // Search Chinese words
    let hits_cn1 = fts.search("高性能", 5).unwrap();
    assert!(!hits_cn1.is_empty(), "Should match Chinese word '高性能'");
    assert_eq!(hits_cn1[0].doc_id, "cn_doc");

    let hits_cn2 = fts.search("数据库", 5).unwrap();
    assert!(!hits_cn2.is_empty(), "Should match Chinese word '数据库'");
    assert_eq!(hits_cn2[0].doc_id, "cn_doc");

    let hits_cn3 = fts.search("分布式数据库", 5).unwrap();
    assert!(!hits_cn3.is_empty(), "Should match compound '分布式数据库'");
    assert_eq!(hits_cn3[0].doc_id, "cn_doc");

    // Search Japanese words
    let hits_jp1 = fts.search("特許", 5).unwrap();
    assert!(!hits_jp1.is_empty(), "Should match Japanese word '特許'");
    assert_eq!(hits_jp1[0].doc_id, "jp_doc");

    let hits_jp2 = fts.search("全文検索", 5).unwrap();
    assert!(
        !hits_jp2.is_empty(),
        "Should match Japanese word '全文検索'"
    );
    assert_eq!(hits_jp2[0].doc_id, "jp_doc");

    // Search Korean words
    let hits_ko1 = fts.search("데이터베이스", 5).unwrap();
    assert!(
        !hits_ko1.is_empty(),
        "Should match Korean stem '데이터베이스'"
    );
    assert_eq!(hits_ko1[0].doc_id, "ko_doc");

    let hits_ko2 = fts.search("전문 검색", 5).unwrap();
    assert!(!hits_ko2.is_empty(), "Should match Korean '전문 검색'");
    assert_eq!(hits_ko2[0].doc_id, "ko_doc");

    // Autocomplete / suggest on CJK
    let sugg_cn = fts.suggest("高性", 1, 5).unwrap();
    assert!(!sugg_cn.is_empty(), "Suggest must work for Chinese prefix");
    assert!(sugg_cn.iter().any(|s| s.term.contains("高性能")));
}

#[test]
fn test_sql_phonetic_and_cjk_functions() {
    let temp = tempdir().unwrap();
    let db_path = temp.path().join("sql_phonetic.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();

    // 1. soundex()
    let res = db
        .sql_query(
            "SELECT soundex('Smith'), soundex('Smyth'), soundex('Gérard'), soundex('Guerard');",
            &[],
        )
        .unwrap();
    assert_eq!(res.rows[0][0], "S530");
    assert_eq!(res.rows[0][1], "S530");
    assert_eq!(res.rows[0][2], "G663");
    assert_eq!(res.rows[0][3], "G663");

    // 2. double_metaphone()
    let res_dm = db
        .sql_query(
            "SELECT double_metaphone('Pfizer'), double_metaphone_primary('Pfizer');",
            &[],
        )
        .unwrap();
    let json_dm = res_dm.rows[0][0].as_str().unwrap();
    assert!(json_dm.contains("FSR"));
    assert_eq!(res_dm.rows[0][1], "FSR");

    // 3. phonetic_match()
    let res_match = db
        .sql_query(
            "SELECT phonetic_match('Gérard', 'Guerard'), phonetic_match('Smith', 'Smyth'), phonetic_match('Pfizer', 'Fizer'), phonetic_match('Smith', 'Johnson');",
            &[],
        )
        .unwrap();
    assert_eq!(res_match.rows[0][0], 1);
    assert_eq!(res_match.rows[0][1], 1);
    assert_eq!(res_match.rows[0][2], 1);
    assert_eq!(res_match.rows[0][3], 0);

    // 4. cjk_segment()
    let res_cjk = db
        .sql_query("SELECT cjk_segment('高性能分布式数据库');", &[])
        .unwrap();
    let segmented = res_cjk.rows[0][0].as_str().unwrap();
    assert!(segmented.contains("高性能"));
    assert!(segmented.contains("数据库"));
}

#[test]
fn test_memory_tier_phonetic_and_cjk() {
    let temp = tempdir().unwrap();
    let db_path = temp.path().join("mem_phonetic.lilia");
    let db = Database::open(DatabaseOptions::durable(&db_path)).unwrap();

    let fts = db.collection("ephem_fts").memory().fts().unwrap();

    fts.index("m1", "Dr. Guerard Smith works at Fizer.")
        .unwrap();
    fts.index("m2", "自然语言处理与多模型数据库。").unwrap();

    // Phonetic on memory tier
    let hits_ph = fts.phonetic_search("Gérard Smyth Pfizer", 5).unwrap();
    assert!(!hits_ph.is_empty());
    assert_eq!(hits_ph[0].doc_id, "m1");

    // CJK on memory tier
    let hits_cjk = fts.search("数据库", 5).unwrap();
    assert!(!hits_cjk.is_empty());
    assert_eq!(hits_cjk[0].doc_id, "m2");
}
