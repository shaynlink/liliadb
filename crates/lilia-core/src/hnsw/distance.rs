//! Distances between stored vectors, for every element type and metric.

use crate::quantization::{
    cosine_distance_f32, cosine_distance_i8, dot_distance_f32, hamming_distance_bits,
    l2_distance_f32,
};
use crate::{VectorElementType, VectorMetric};

pub(super) fn compute_vector_distance(
    query: &[u8],
    target: &[u8],
    element_type: VectorElementType,
    metric: VectorMetric,
) -> f64 {
    match element_type {
        VectorElementType::Float32 => {
            let q = bytes_to_f32_slice(query);
            let t = bytes_to_f32_slice(target);
            match metric {
                VectorMetric::Cosine => cosine_distance_f32(&q, &t),
                VectorMetric::L2 => l2_distance_f32(&q, &t),
                VectorMetric::Dot => dot_distance_f32(&q, &t),
                VectorMetric::Hamming => f64::from(hamming_distance_bits(query, target)),
            }
        }
        VectorElementType::Int8 => {
            let q = bytes_to_i8_vec(query);
            let t = bytes_to_i8_vec(target);
            match metric {
                VectorMetric::Cosine => cosine_distance_i8(&q, &t),
                VectorMetric::Hamming => f64::from(hamming_distance_bits(query, target)),
                VectorMetric::L2 | VectorMetric::Dot => {
                    let q_f: Vec<f32> = q.iter().map(|&x| f32::from(x)).collect();
                    let t_f: Vec<f32> = t.iter().map(|&x| f32::from(x)).collect();
                    if metric == VectorMetric::L2 {
                        l2_distance_f32(&q_f, &t_f)
                    } else {
                        dot_distance_f32(&q_f, &t_f)
                    }
                }
            }
        }
        VectorElementType::Bit => f64::from(hamming_distance_bits(query, target)),
    }
}

fn bytes_to_f32_slice(bytes: &[u8]) -> Vec<f32> {
    if !bytes.len().is_multiple_of(4) {
        return Vec::new();
    }
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn bytes_to_i8_vec(bytes: &[u8]) -> Vec<i8> {
    bytes.iter().map(|&b| i8::from_ne_bytes([b])).collect()
}
