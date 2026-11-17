//! Query results and their conversion to JSON rows.

use datafusion::arrow::array::{Array, ArrayRef, AsArray, RecordBatch};
use datafusion::arrow::compute::cast;
use datafusion::arrow::datatypes::{DataType, Float64Type, Int64Type, SchemaRef, UInt64Type};
use datafusion::arrow::util::display::{ArrayFormatter, FormatOptions};
use serde_json::{Number, Value};

use crate::error::Result;

/// Result of an analytical query: Arrow batches plus the schema, which is available
/// even when the query returns no row.
#[derive(Debug, Clone)]
pub struct QueryOutput {
    pub schema: SchemaRef,
    pub batches: Vec<RecordBatch>,
}

impl QueryOutput {
    /// Total number of rows across all batches.
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.batches.iter().map(RecordBatch::num_rows).sum()
    }

    /// Output column names, in order.
    #[must_use]
    pub fn column_names(&self) -> Vec<String> {
        self.schema
            .fields()
            .iter()
            .map(|field| field.name().clone())
            .collect()
    }

    /// Converts the result to row-oriented JSON values.
    ///
    /// Integers, floats, booleans and strings map to their JSON counterparts; a
    /// non-finite float becomes `null`; any other type (timestamps, decimals, lists…)
    /// is rendered as its Arrow display string.
    ///
    /// # Errors
    /// Returns an Arrow error if a column cannot be cast or formatted.
    pub fn to_json_rows(&self) -> Result<Vec<Vec<Value>>> {
        let mut rows = Vec::with_capacity(self.row_count());
        for batch in &self.batches {
            let first_row = rows.len();
            rows.resize_with(first_row + batch.num_rows(), || {
                Vec::with_capacity(batch.num_columns())
            });
            for column in batch.columns() {
                for (row, value) in rows[first_row..].iter_mut().zip(column_values(column)?) {
                    row.push(value);
                }
            }
        }
        Ok(rows)
    }
}

fn column_values(column: &ArrayRef) -> Result<Vec<Value>> {
    let values = match column.data_type() {
        DataType::Null => vec![Value::Null; column.len()],
        DataType::Boolean => column
            .as_boolean()
            .iter()
            .map(|value| value.map_or(Value::Null, Value::Bool))
            .collect(),
        DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64 => {
            cast(column, &DataType::Int64)?
                .as_primitive::<Int64Type>()
                .iter()
                .map(|value| value.map_or(Value::Null, Value::from))
                .collect()
        }
        DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64 => {
            cast(column, &DataType::UInt64)?
                .as_primitive::<UInt64Type>()
                .iter()
                .map(|value| value.map_or(Value::Null, Value::from))
                .collect()
        }
        DataType::Float16 | DataType::Float32 | DataType::Float64 => {
            cast(column, &DataType::Float64)?
                .as_primitive::<Float64Type>()
                .iter()
                .map(|value| {
                    value
                        .and_then(Number::from_f64)
                        .map_or(Value::Null, Value::Number)
                })
                .collect()
        }
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => cast(column, &DataType::Utf8)?
            .as_string::<i32>()
            .iter()
            .map(|value| value.map_or(Value::Null, |text| Value::String(text.to_owned())))
            .collect(),
        _ => {
            let options = FormatOptions::default();
            let formatter = ArrayFormatter::try_new(column.as_ref(), &options)?;
            (0..column.len())
                .map(|index| {
                    if column.is_null(index) {
                        Value::Null
                    } else {
                        Value::String(formatter.value(index).to_string())
                    }
                })
                .collect()
        }
    };
    Ok(values)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use datafusion::arrow::array::{
        BooleanArray, Float64Array, Int32Array, StringArray, TimestampMillisecondArray, UInt64Array,
    };
    use datafusion::arrow::datatypes::{Field, Schema, TimeUnit};
    use serde_json::json;

    use super::*;

    #[test]
    fn converts_every_column_kind_to_json_rows() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("i", DataType::Int32, true),
            Field::new("u", DataType::UInt64, true),
            Field::new("f", DataType::Float64, true),
            Field::new("b", DataType::Boolean, true),
            Field::new("s", DataType::Utf8, true),
            Field::new("t", DataType::Timestamp(TimeUnit::Millisecond, None), true),
        ]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int32Array::from(vec![Some(-3), None])),
                Arc::new(UInt64Array::from(vec![Some(u64::MAX), None])),
                Arc::new(Float64Array::from(vec![Some(1.5), Some(f64::NAN)])),
                Arc::new(BooleanArray::from(vec![Some(true), None])),
                Arc::new(StringArray::from(vec![Some("paris"), None])),
                Arc::new(TimestampMillisecondArray::from(vec![Some(0), None])),
            ],
        )
        .unwrap();
        let output = QueryOutput {
            schema,
            batches: vec![batch.clone(), batch],
        };

        assert_eq!(output.row_count(), 4);
        assert_eq!(output.column_names(), ["i", "u", "f", "b", "s", "t"]);
        let rows = output.to_json_rows().unwrap();
        assert_eq!(rows.len(), 4);
        assert_eq!(
            rows[0],
            vec![
                json!(-3),
                json!(u64::MAX),
                json!(1.5),
                json!(true),
                json!("paris"),
                json!("1970-01-01T00:00:00")
            ]
        );
        assert_eq!(rows[1], vec![Value::Null; 6]);
        assert_eq!(rows[2], rows[0]);
    }

    #[test]
    fn empty_result_keeps_its_columns() {
        let schema = Arc::new(Schema::new(vec![Field::new(
            "total",
            DataType::Int64,
            true,
        )]));
        let output = QueryOutput {
            schema,
            batches: Vec::new(),
        };
        assert_eq!(output.row_count(), 0);
        assert_eq!(output.column_names(), ["total"]);
        assert_eq!(output.to_json_rows().unwrap(), Vec::<Vec<Value>>::new());
    }
}
