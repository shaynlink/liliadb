//! Vectors: validated values, their record encoding, conversions and exact distances.

use bytes::Bytes;
use lilia_core::quantization::{
    cosine_distance_f32, cosine_distance_i8, dot_distance_f32, hamming_distance_bits,
    l2_distance_f32, quantize_f32_to_bits, quantize_f32_to_i8,
};
use lilia_core::{VectorElementType, VectorMetric};

use crate::error::{corruption, invalid, Result};

const FORMAT: u8 = 1;
/// Largest dimension accepted.
pub const MAX_DIM: u32 = 1 << 16;

/// A vector of `dim` elements: little-endian `f32`, `i8`, or bits packed most significant
/// first.
#[derive(Debug, Clone, PartialEq)]
pub struct Vector {
    element_type: VectorElementType,
    dim: u32,
    data: Vec<u8>,
}

fn byte_len(element_type: VectorElementType, dim: u32) -> usize {
    let dim = dim as usize;
    match element_type {
        VectorElementType::Float32 => dim * 4,
        VectorElementType::Int8 => dim,
        VectorElementType::Bit => dim.div_ceil(8),
    }
}

fn element_type_of(code: u8) -> Result<VectorElementType> {
    VectorElementType::try_from(i64::from(code))
        .map_err(|_| corruption("vector record with an unknown element type"))
}

impl Vector {
    /// A vector from its raw bytes.
    ///
    /// # Errors
    ///
    /// [`crate::Error::InvalidArgument`] for a dimension of 0 or above 65,536, bytes that do not
    /// hold `dim` elements, or a `f32` element that is not finite.
    pub fn new(element_type: VectorElementType, dim: u32, data: Vec<u8>) -> Result<Self> {
        if dim == 0 || dim > MAX_DIM {
            return Err(invalid(format!(
                "vector dimensions must be between 1 and {MAX_DIM}, got {dim}"
            )));
        }
        let expected = byte_len(element_type, dim);
        if data.len() != expected {
            return Err(invalid(format!(
                "a {} vector of dimension {dim} holds {expected} bytes, got {}",
                element_type.as_str(),
                data.len()
            )));
        }
        let vector = Self {
            element_type,
            dim,
            data,
        };
        if element_type == VectorElementType::Float32
            && vector.floats().iter().any(|x| !x.is_finite())
        {
            return Err(invalid("vector elements must be finite"));
        }
        Ok(vector)
    }

    /// # Errors
    ///
    /// As [`Vector::new`].
    pub fn from_f32(values: &[f32]) -> Result<Self> {
        let dim = u32::try_from(values.len()).map_err(|_| invalid("too many vector elements"))?;
        Self::new(
            VectorElementType::Float32,
            dim,
            values.iter().flat_map(|x| x.to_le_bytes()).collect(),
        )
    }

    /// # Errors
    ///
    /// As [`Vector::new`].
    pub fn from_i8(values: &[i8]) -> Result<Self> {
        let dim = u32::try_from(values.len()).map_err(|_| invalid("too many vector elements"))?;
        Self::new(
            VectorElementType::Int8,
            dim,
            values.iter().map(|&x| x.cast_unsigned()).collect(),
        )
    }

    #[must_use]
    pub fn element_type(&self) -> VectorElementType {
        self.element_type
    }

    #[must_use]
    pub fn dim(&self) -> u32 {
        self.dim
    }

    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// The elements of a `f32` vector; empty for other types.
    #[must_use]
    pub fn floats(&self) -> Vec<f32> {
        if self.element_type != VectorElementType::Float32 {
            return Vec::new();
        }
        self.data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect()
    }

    fn ints(&self) -> Vec<i8> {
        self.data.iter().map(|&b| b.cast_signed()).collect()
    }

    /// The vector as `target` elements: `f32` quantized to `i8` or bits, other types as they
    /// are. `None` when there is no conversion.
    #[must_use]
    pub fn convert(&self, target: VectorElementType) -> Option<Self> {
        let data = match (self.element_type, target) {
            (a, b) if a == b => self.data.clone(),
            (VectorElementType::Float32, VectorElementType::Int8) => {
                quantize_f32_to_i8(&self.floats())
                    .0
                    .into_iter()
                    .map(i8::cast_unsigned)
                    .collect()
            }
            (VectorElementType::Float32, VectorElementType::Bit) => {
                quantize_f32_to_bits(&self.floats())
            }
            (VectorElementType::Int8, VectorElementType::Bit) => {
                quantize_f32_to_bits(&self.ints().into_iter().map(f32::from).collect::<Vec<_>>())
            }
            _ => return None,
        };
        Some(Self {
            element_type: target,
            dim: self.dim,
            data,
        })
    }

    #[must_use]
    pub fn encode(&self) -> Bytes {
        let mut out = Vec::with_capacity(self.data.len() + 6);
        out.push(FORMAT);
        out.push(self.element_type as u8);
        out.extend_from_slice(&self.dim.to_le_bytes());
        out.extend_from_slice(&self.data);
        out.into()
    }

    /// # Errors
    ///
    /// [`crate::Error::Corruption`] for a record that does not decode.
    pub fn decode(raw: &[u8]) -> Result<Self> {
        let [FORMAT, code, d0, d1, d2, d3, data @ ..] = raw else {
            return Err(corruption("vector record with an unknown format"));
        };
        Self::new(
            element_type_of(*code)?,
            u32::from_le_bytes([*d0, *d1, *d2, *d3]),
            data.to_vec(),
        )
        .map_err(|e| corruption(format!("vector record: {e}")))
    }
}

/// Distance from `query` to `stored` under `metric`, computed in the stored vector's element
/// type (a `f32` query is quantized for an `i8` or bit vector). Bit vectors are compared by
/// Hamming distance whatever the metric. `None` when the dimensions differ or the query cannot
/// be converted.
#[must_use]
pub fn distance(query: &Vector, stored: &Vector, metric: VectorMetric) -> Option<f64> {
    if query.dim != stored.dim {
        return None;
    }
    if metric == VectorMetric::Hamming || stored.element_type == VectorElementType::Bit {
        let q = query.convert(VectorElementType::Bit)?;
        let s = stored.convert(VectorElementType::Bit)?;
        return Some(f64::from(hamming_distance_bits(&q.data, &s.data)));
    }
    let q = query.convert(stored.element_type)?;
    Some(match stored.element_type {
        VectorElementType::Float32 => {
            let (a, b) = (q.floats(), stored.floats());
            match metric {
                VectorMetric::Cosine => cosine_distance_f32(&a, &b),
                VectorMetric::L2 => l2_distance_f32(&a, &b),
                VectorMetric::Dot | VectorMetric::Hamming => dot_distance_f32(&a, &b),
            }
        }
        VectorElementType::Int8 => {
            let (a, b) = (q.ints(), stored.ints());
            let float = |v: &[i8]| v.iter().map(|&x| f32::from(x)).collect::<Vec<_>>();
            match metric {
                VectorMetric::Cosine => cosine_distance_i8(&a, &b),
                VectorMetric::L2 => l2_distance_f32(&float(&a), &float(&b)),
                VectorMetric::Dot | VectorMetric::Hamming => {
                    dot_distance_f32(&float(&a), &float(&b))
                }
            }
        }
        VectorElementType::Bit => f64::from(hamming_distance_bits(&q.data, &stored.data)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vectors_validate_round_trip_and_measure() {
        let v = Vector::from_f32(&[1.0, 0.0, -2.0]).unwrap();
        assert_eq!(Vector::decode(&v.encode()).unwrap(), v);
        assert!(Vector::from_f32(&[]).is_err());
        assert!(Vector::from_f32(&[f32::NAN]).is_err());
        assert!(Vector::new(VectorElementType::Bit, 9, vec![0]).is_err());
        assert_eq!(
            Vector::new(VectorElementType::Bit, 9, vec![0, 0])
                .unwrap()
                .dim(),
            9
        );
        assert!(Vector::decode(&[2, 0]).is_err());

        let w = Vector::from_f32(&[1.0, 0.0, -2.0]).unwrap();
        assert!(distance(&v, &w, VectorMetric::Cosine).unwrap().abs() < 1e-9);
        assert!(distance(&v, &w, VectorMetric::L2).unwrap().abs() < 1e-9);
        let q = v.convert(VectorElementType::Int8).unwrap();
        assert_eq!(q.element_type(), VectorElementType::Int8);
        assert!(
            distance(&v, &q, VectorMetric::Cosine).unwrap() < 1e-3,
            "a f32 query is quantized"
        );
        let bits = v.convert(VectorElementType::Bit).unwrap();
        assert_eq!(bits.data(), [0b1000_0000]);
        assert_eq!(distance(&v, &bits, VectorMetric::Cosine), Some(0.0));
        assert_eq!(
            distance(&Vector::from_f32(&[1.0]).unwrap(), &v, VectorMetric::L2),
            None
        );
        assert_eq!(q.convert(VectorElementType::Float32), None);
    }
}
