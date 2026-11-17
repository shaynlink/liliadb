//! Persistent columnar segments: one Arrow IPC chunk per column plus zone maps.

use std::cmp::Ordering;
use std::sync::Arc;

use datafusion::arrow::array::{Array, ArrayRef, AsArray, RecordBatch};
use datafusion::arrow::compute::{max, max_boolean, max_string, min, min_boolean, min_string};
use datafusion::arrow::datatypes::{
    DataType, Field, Float64Type, Int64Type, Schema, TimeUnit, TimestampMillisecondType,
};
use datafusion::arrow::ipc::reader::StreamReader;
use datafusion::arrow::ipc::writer::StreamWriter;
use serde::{Deserialize, Serialize};

use crate::error::{AnalyticsError, Result};

/// Largest integer magnitude a `f64` represents exactly.
const MAX_EXACT_F64_INTEGER: u64 = 1 << 53;

/// A single comparable value: a zone-map bound or a predicate literal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum Scalar {
    Int(i64),
    Float(f64),
    Bool(bool),
    Text(String),
}

impl Scalar {
    /// Orders two scalars, or returns `None` when they are not comparable
    /// (different kinds, a NaN, or an integer too large to compare exactly with a float).
    #[must_use]
    pub fn compare(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => Some(a.cmp(b)),
            (Self::Float(a), Self::Float(b)) => a.partial_cmp(b),
            (Self::Int(a), Self::Float(b)) => exact_f64(*a)?.partial_cmp(b),
            (Self::Float(a), Self::Int(b)) => a.partial_cmp(&exact_f64(*b)?),
            (Self::Bool(a), Self::Bool(b)) => Some(a.cmp(b)),
            (Self::Text(a), Self::Text(b)) => Some(a.as_str().cmp(b.as_str())),
            _ => None,
        }
    }
}

/// Converts an integer to `f64` only when the conversion is exact.
fn exact_f64(value: i64) -> Option<f64> {
    if value.unsigned_abs() > MAX_EXACT_F64_INTEGER {
        return None;
    }
    // Lossless: the magnitude was just checked to fit the 53-bit mantissa.
    #[allow(clippy::cast_precision_loss)]
    let exact = value as f64;
    Some(exact)
}

/// Comparison operator of a pushed-down predicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Lt,
    LtEq,
    Gt,
    GtEq,
}

/// `field <op> value`, where `field` indexes the table schema (`0` is the key column).
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnPredicate {
    pub field: usize,
    pub op: CmpOp,
    pub value: Scalar,
}

/// Min/max summary of one column chunk, used to skip segments that cannot match.
///
/// `min`/`max` are `None` when the bounds are unknown (unsupported type or non-finite
/// floats); such a zone map never prunes unless the chunk is entirely null.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ZoneMap {
    pub min: Option<Scalar>,
    pub max: Option<Scalar>,
    pub null_count: usize,
    pub row_count: usize,
}

impl ZoneMap {
    /// Computes the zone map of a column chunk.
    #[must_use]
    pub fn from_array(array: &ArrayRef) -> Self {
        let (min, max) = bounds(array);
        Self {
            min,
            max,
            null_count: array.null_count(),
            row_count: array.len(),
        }
    }

    /// Returns `false` only when no row of the chunk can satisfy `column <op> value`.
    #[must_use]
    pub fn may_match(&self, op: CmpOp, value: &Scalar) -> bool {
        if self.null_count >= self.row_count {
            // A comparison against NULL is never true.
            return false;
        }
        let (Some(low), Some(high)) = (&self.min, &self.max) else {
            return true;
        };
        let low_vs_value = low.compare(value);
        let high_vs_value = high.compare(value);
        let excluded = match op {
            CmpOp::Eq => {
                low_vs_value == Some(Ordering::Greater) || high_vs_value == Some(Ordering::Less)
            }
            CmpOp::Lt => matches!(low_vs_value, Some(Ordering::Greater | Ordering::Equal)),
            CmpOp::LtEq => low_vs_value == Some(Ordering::Greater),
            CmpOp::Gt => matches!(high_vs_value, Some(Ordering::Less | Ordering::Equal)),
            CmpOp::GtEq => high_vs_value == Some(Ordering::Less),
        };
        !excluded
    }
}

fn bounds(array: &ArrayRef) -> (Option<Scalar>, Option<Scalar>) {
    match array.data_type() {
        DataType::Int64 => {
            let values = array.as_primitive::<Int64Type>();
            (min(values).map(Scalar::Int), max(values).map(Scalar::Int))
        }
        DataType::Timestamp(TimeUnit::Millisecond, _) => {
            let values = array.as_primitive::<TimestampMillisecondType>();
            (min(values).map(Scalar::Int), max(values).map(Scalar::Int))
        }
        DataType::Float64 => {
            let values = array.as_primitive::<Float64Type>();
            match (min(values), max(values)) {
                (Some(low), Some(high)) if low.is_finite() && high.is_finite() => {
                    (Some(Scalar::Float(low)), Some(Scalar::Float(high)))
                }
                _ => (None, None),
            }
        }
        DataType::Boolean => {
            let values = array.as_boolean();
            (
                min_boolean(values).map(Scalar::Bool),
                max_boolean(values).map(Scalar::Bool),
            )
        }
        DataType::Utf8 => {
            let values = array.as_string::<i32>();
            (
                min_string(values).map(|text| Scalar::Text(text.to_owned())),
                max_string(values).map(|text| Scalar::Text(text.to_owned())),
            )
        }
        _ => (None, None),
    }
}

/// Persisted description of one segment: its row count and one zone map per field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SegmentMeta {
    pub row_count: usize,
    /// Indexed like the table schema: `zones[0]` covers the key column.
    pub zones: Vec<ZoneMap>,
}

impl SegmentMeta {
    /// Summarizes a full-width segment batch (every field of the table schema).
    #[must_use]
    pub fn from_batch(batch: &RecordBatch) -> Self {
        Self {
            row_count: batch.num_rows(),
            zones: batch.columns().iter().map(ZoneMap::from_array).collect(),
        }
    }

    /// Smallest key stored in the segment.
    #[must_use]
    pub fn min_key(&self) -> Option<&Scalar> {
        self.zones.first().and_then(|zone| zone.min.as_ref())
    }

    /// Largest key stored in the segment.
    #[must_use]
    pub fn max_key(&self) -> Option<&Scalar> {
        self.zones.first().and_then(|zone| zone.max.as_ref())
    }

    /// Returns `false` only when no row of the segment can satisfy every predicate.
    #[must_use]
    pub fn may_match(&self, predicates: &[ColumnPredicate]) -> bool {
        predicates.iter().all(|predicate| {
            self.zones
                .get(predicate.field)
                .is_none_or(|zone| zone.may_match(predicate.op, &predicate.value))
        })
    }

    /// Serializes the metadata for persistence.
    ///
    /// # Errors
    /// Returns [`AnalyticsError::Corrupt`] if a bound cannot be represented.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self)
            .map_err(|e| AnalyticsError::Corrupt(format!("segment metadata encoding: {e}")))
    }

    /// Reads metadata written by [`SegmentMeta::to_bytes`].
    ///
    /// # Errors
    /// Returns [`AnalyticsError::Corrupt`] when the bytes are not valid metadata.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes)
            .map_err(|e| AnalyticsError::Corrupt(format!("segment metadata decoding: {e}")))
    }
}

/// Encodes one column of a segment as a self-describing Arrow IPC stream.
///
/// # Errors
/// Returns an Arrow error if the array cannot be written.
pub fn encode_chunk(array: &ArrayRef) -> Result<Vec<u8>> {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "c",
        array.data_type().clone(),
        true,
    )]));
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![Arc::clone(array)])?;
    let mut writer = StreamWriter::try_new(Vec::new(), &schema)?;
    writer.write(&batch)?;
    writer.finish()?;
    Ok(writer.into_inner()?)
}

/// Decodes a chunk written by [`encode_chunk`], checking its type and length.
///
/// # Errors
/// Returns [`AnalyticsError::Corrupt`] when the chunk is not a single column of
/// `expected_type` holding exactly `expected_rows` values.
pub fn decode_chunk(
    bytes: &[u8],
    expected_type: &DataType,
    expected_rows: usize,
) -> Result<ArrayRef> {
    let corrupt = |reason: String| AnalyticsError::Corrupt(format!("column chunk {reason}"));
    let mut reader = StreamReader::try_new(bytes, None)
        .map_err(|e| corrupt(format!("is not an Arrow IPC stream: {e}")))?;
    let batch = reader
        .next()
        .ok_or_else(|| corrupt("holds no record batch".into()))?
        .map_err(|e| corrupt(format!("cannot be decoded: {e}")))?;
    if reader.next().is_some() {
        return Err(corrupt("holds more than one record batch".into()));
    }
    if batch.num_columns() != 1 {
        return Err(corrupt(format!(
            "holds {} columns instead of 1",
            batch.num_columns()
        )));
    }
    let array = Arc::clone(batch.column(0));
    if array.data_type() != expected_type {
        return Err(corrupt(format!(
            "has type {} instead of {expected_type}",
            array.data_type()
        )));
    }
    if array.len() != expected_rows {
        return Err(corrupt(format!(
            "holds {} rows instead of {expected_rows}",
            array.len()
        )));
    }
    Ok(array)
}

#[cfg(test)]
mod tests {
    use datafusion::arrow::array::{
        BooleanArray, Float64Array, Int64Array, StringArray, TimestampMillisecondArray,
    };

    use super::*;

    fn int_zone(values: Vec<Option<i64>>) -> ZoneMap {
        ZoneMap::from_array(&(Arc::new(Int64Array::from(values)) as ArrayRef))
    }

    #[test]
    fn chunks_round_trip_for_every_supported_type() {
        let arrays: Vec<ArrayRef> = vec![
            Arc::new(Int64Array::from(vec![Some(1), None, Some(-7)])),
            Arc::new(Float64Array::from(vec![Some(1.5), None, Some(f64::NAN)])),
            Arc::new(BooleanArray::from(vec![Some(true), None, Some(false)])),
            Arc::new(StringArray::from(vec![Some("paris"), None, Some("")])),
            Arc::new(TimestampMillisecondArray::from(vec![
                Some(10),
                None,
                Some(3),
            ])),
        ];
        for array in arrays {
            let bytes = encode_chunk(&array).unwrap();
            let decoded = decode_chunk(&bytes, array.data_type(), 3).unwrap();
            assert_eq!(decoded.to_data(), array.to_data());
        }
    }

    #[test]
    fn decoding_rejects_wrong_shape_or_garbage() {
        let array: ArrayRef = Arc::new(Int64Array::from(vec![1, 2, 3]));
        let bytes = encode_chunk(&array).unwrap();
        assert!(decode_chunk(&bytes, &DataType::Float64, 3).is_err());
        assert!(decode_chunk(&bytes, &DataType::Int64, 4).is_err());
        assert!(decode_chunk(b"not arrow", &DataType::Int64, 3).is_err());
        assert!(decode_chunk(&bytes[..bytes.len() / 2], &DataType::Int64, 3).is_err());
    }

    #[test]
    fn zone_map_prunes_only_impossible_ranges() {
        let zone = int_zone(vec![Some(10), None, Some(20)]);
        assert_eq!(zone.min, Some(Scalar::Int(10)));
        assert_eq!(zone.max, Some(Scalar::Int(20)));
        assert_eq!(zone.null_count, 1);

        assert!(zone.may_match(CmpOp::Eq, &Scalar::Int(10)));
        assert!(zone.may_match(CmpOp::Eq, &Scalar::Int(15)));
        assert!(!zone.may_match(CmpOp::Eq, &Scalar::Int(9)));
        assert!(!zone.may_match(CmpOp::Eq, &Scalar::Int(21)));

        assert!(zone.may_match(CmpOp::Gt, &Scalar::Int(19)));
        assert!(!zone.may_match(CmpOp::Gt, &Scalar::Int(20)));
        assert!(zone.may_match(CmpOp::GtEq, &Scalar::Int(20)));
        assert!(!zone.may_match(CmpOp::GtEq, &Scalar::Int(21)));

        assert!(zone.may_match(CmpOp::Lt, &Scalar::Int(11)));
        assert!(!zone.may_match(CmpOp::Lt, &Scalar::Int(10)));
        assert!(zone.may_match(CmpOp::LtEq, &Scalar::Int(10)));
        assert!(!zone.may_match(CmpOp::LtEq, &Scalar::Int(9)));
    }

    #[test]
    fn zone_map_compares_integers_with_floats() {
        let zone = int_zone(vec![Some(10), Some(20)]);
        assert!(!zone.may_match(CmpOp::Gt, &Scalar::Float(20.0)));
        assert!(zone.may_match(CmpOp::Gt, &Scalar::Float(19.5)));
        // Incomparable or unknown operands must never prune.
        assert!(zone.may_match(CmpOp::Gt, &Scalar::Float(f64::NAN)));
        assert!(zone.may_match(CmpOp::Eq, &Scalar::Text("x".into())));
        let huge = int_zone(vec![Some(i64::MAX)]);
        assert!(huge.may_match(CmpOp::Lt, &Scalar::Float(1.0)));
    }

    #[test]
    fn all_null_and_non_finite_chunks_are_handled_conservatively() {
        let nulls = int_zone(vec![None, None]);
        assert!(!nulls.may_match(CmpOp::Eq, &Scalar::Int(1)));

        let with_nan: ArrayRef = Arc::new(Float64Array::from(vec![1.0, f64::NAN]));
        let zone = ZoneMap::from_array(&with_nan);
        assert_eq!((zone.min.clone(), zone.max.clone()), (None, None));
        assert!(zone.may_match(CmpOp::Gt, &Scalar::Float(1e300)));
    }

    #[test]
    fn segment_meta_round_trips_and_combines_predicates() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new("age", DataType::Int64, true),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(vec!["a", "m"])),
                Arc::new(Int64Array::from(vec![30, 40])),
            ],
        )
        .unwrap();
        let meta = SegmentMeta::from_batch(&batch);
        assert_eq!(meta.min_key(), Some(&Scalar::Text("a".into())));
        assert_eq!(meta.max_key(), Some(&Scalar::Text("m".into())));
        assert_eq!(
            SegmentMeta::from_bytes(&meta.to_bytes().unwrap()).unwrap(),
            meta
        );
        assert!(SegmentMeta::from_bytes(b"{").is_err());

        let predicate = |field, op, value| ColumnPredicate { field, op, value };
        assert!(meta.may_match(&[predicate(1, CmpOp::Gt, Scalar::Int(35))]));
        assert!(!meta.may_match(&[
            predicate(1, CmpOp::Gt, Scalar::Int(35)),
            predicate(0, CmpOp::Gt, Scalar::Text("z".into())),
        ]));
        // A predicate on an unknown field cannot prune.
        assert!(meta.may_match(&[predicate(9, CmpOp::Eq, Scalar::Int(0))]));
    }
}
