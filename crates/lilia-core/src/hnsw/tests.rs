use super::*;
use crate::{VectorElementType, VectorMetric};

fn to_bytes(floats: &[f32]) -> Vec<u8> {
    let mut b = Vec::with_capacity(floats.len() * 4);
    for &f in floats {
        b.extend_from_slice(&f.to_le_bytes());
    }
    b
}

#[test]
fn test_hnsw_basic_insert_search_and_recall() {
    let config = HnswConfig {
        m: 16,
        ef_construction: 64,
        ef_search: 32,
        metric: VectorMetric::Cosine,
        element_type: VectorElementType::Float32,
        dim: 4,
    };
    let mut index = HnswIndex::new(config);

    let v1 = to_bytes(&[1.0, 0.0, 0.0, 0.0]);
    let v2 = to_bytes(&[0.0, 1.0, 0.0, 0.0]);
    let v3 = to_bytes(&[0.9, 0.1, 0.0, 0.0]);
    let v4 = to_bytes(&[0.0, 0.0, 1.0, 0.0]);

    index.insert("d1", &v1).unwrap();
    index.insert("d2", &v2).unwrap();
    index.insert("d3", &v3).unwrap();
    index.insert("d4", &v4).unwrap();

    assert_eq!(index.len(), 4);

    // Query close to d1 and d3
    let q = to_bytes(&[0.95, 0.05, 0.0, 0.0]);
    let hits = index.search("test", &q, 2, None, None);

    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].id, "d1");
    assert_eq!(hits[1].id, "d3");
    assert!(hits[0].distance < hits[1].distance);
}

#[test]
fn test_hnsw_pre_filtered_search() {
    let config = HnswConfig {
        m: 16,
        ef_construction: 64,
        ef_search: 32,
        metric: VectorMetric::Cosine,
        element_type: VectorElementType::Float32,
        dim: 3,
    };
    let mut index = HnswIndex::new(config);

    // d1 is nearest to query, but category is "books"
    // d2 is further, but category is "electronics"
    // d3 is far, category is "electronics"
    index.insert("d1", &to_bytes(&[1.0, 0.0, 0.0])).unwrap();
    index.insert("d2", &to_bytes(&[0.8, 0.2, 0.0])).unwrap();
    index.insert("d3", &to_bytes(&[0.0, 1.0, 0.0])).unwrap();

    let query = to_bytes(&[1.0, 0.0, 0.0]);

    // Unfiltered search: top 1 is d1
    let hits_unfiltered = index.search("items", &query, 1, None, None);
    assert_eq!(hits_unfiltered[0].id, "d1");

    // Filtered search: only "electronics" allowed (d2 and d3)
    let filter = |id: &str| id == "d2" || id == "d3";
    let hits_filtered = index.search("items", &query, 2, None, Some(&filter));

    assert_eq!(hits_filtered.len(), 2);
    assert_eq!(hits_filtered[0].id, "d2");
    assert_eq!(hits_filtered[1].id, "d3");
}

#[test]
fn test_hnsw_quantized_sq8_and_rerank() {
    let config = HnswConfig {
        m: 16,
        ef_construction: 64,
        ef_search: 32,
        metric: VectorMetric::Cosine,
        element_type: VectorElementType::Int8,
        dim: 3,
    };
    let mut index = HnswIndex::new(config);

    let orig_v1 = vec![1.0f32, 0.0, 0.0];
    let orig_v2 = vec![0.8f32, 0.2, 0.0];
    let orig_v3 = vec![0.0f32, 1.0, 0.0];

    let (q1, _) = crate::quantization::quantize_f32_to_i8(&orig_v1);
    let (q2, _) = crate::quantization::quantize_f32_to_i8(&orig_v2);
    let (q3, _) = crate::quantization::quantize_f32_to_i8(&orig_v3);

    let b1: Vec<u8> = q1.iter().map(|&x| x.cast_unsigned()).collect();
    let b2: Vec<u8> = q2.iter().map(|&x| x.cast_unsigned()).collect();
    let b3: Vec<u8> = q3.iter().map(|&x| x.cast_unsigned()).collect();

    index.insert("item1", &b1).unwrap();
    index.insert("item2", &b2).unwrap();
    index.insert("item3", &b3).unwrap();

    let query_f32 = vec![0.95f32, 0.05, 0.0];
    let (query_i8, _) = crate::quantization::quantize_f32_to_i8(&query_f32);
    let query_bytes: Vec<u8> = query_i8.iter().map(|&x| x.cast_unsigned()).collect();

    let raw_store: HashMap<String, Vec<f32>> = [
        ("item1".into(), orig_v1),
        ("item2".into(), orig_v2),
        ("item3".into(), orig_v3),
    ]
    .into_iter()
    .collect();

    let reranked =
        index.search_and_rerank("store", &query_f32, &query_bytes, 2, None, None, 2, |id| {
            raw_store.get(id).cloned()
        });

    assert_eq!(reranked.len(), 2);
    assert_eq!(reranked[0].id, "item1");
    assert_eq!(reranked[1].id, "item2");
}

#[test]
fn test_hnsw_deletion_and_repair() {
    let config = HnswConfig {
        m: 16,
        ef_construction: 64,
        ef_search: 32,
        metric: VectorMetric::Cosine,
        element_type: VectorElementType::Float32,
        dim: 2,
    };
    let mut index = HnswIndex::new(config);

    index.insert("a", &to_bytes(&[1.0, 0.0])).unwrap();
    index.insert("b", &to_bytes(&[0.9, 0.1])).unwrap();
    index.insert("c", &to_bytes(&[0.0, 1.0])).unwrap();

    assert_eq!(index.len(), 3);
    assert!(index.delete("a"));
    assert_eq!(index.len(), 2);
    assert!(!index.contains("a"));

    let hits = index.search("items", &to_bytes(&[1.0, 0.0]), 2, None, None);
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].id, "b");
    assert_eq!(hits[1].id, "c");
}
