#![cfg(test)]

use serde_json::json;

use super::BinaryDoc;

#[test]
fn test_binary_doc_roundtrip_primitives() {
    let val = json!({
        "name": "Alice",
        "age": 30,
        "active": true,
        "score": 98.5,
        "notes": null,
        "tags": ["admin", "developer"],
        "nested": {
            "deep": {
                "city": "Paris"
            }
        }
    });

    let doc = BinaryDoc::from_json_value(&val).unwrap();
    assert_eq!(doc.extract_str("name"), Some("Alice"));
    assert_eq!(doc.extract_i64("age"), Some(30));
    assert_eq!(doc.extract_bool("active"), Some(true));
    assert_eq!(doc.extract_f64("score"), Some(98.5));
    assert!(doc.extract("notes").unwrap().is_null());

    // Nested zero-copy extraction
    assert_eq!(doc.extract_str("nested.deep.city"), Some("Paris"));
    assert_eq!(doc.extract_str("/nested/deep/city"), Some("Paris"));
    assert_eq!(doc.extract_str("$.nested.deep.city"), Some("Paris"));

    // Array indexing
    assert_eq!(doc.extract_str("tags.0"), Some("admin"));
    assert_eq!(doc.extract_str("tags[1]"), Some("developer"));

    // Full JSON roundtrip
    let restored = doc.to_json_value();
    assert_eq!(restored, val);
}

#[test]
fn test_binary_search_large_object() {
    let mut map = serde_json::Map::new();
    for i in 0..200 {
        map.insert(format!("field_{i:03}"), json!(i * 10));
    }
    let doc = BinaryDoc::from_json_value(&serde_json::Value::Object(map)).unwrap();

    assert_eq!(doc.extract_i64("field_042"), Some(420));
    assert_eq!(doc.extract_i64("field_199"), Some(1990));
    assert_eq!(doc.extract("field_999"), None);
}
