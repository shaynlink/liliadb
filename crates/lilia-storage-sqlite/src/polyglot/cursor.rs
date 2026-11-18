use std::sync::Arc;

use arrow::array::{Array, AsArray};
use arrow::datatypes::*;
use arrow::record_batch::RecordBatch;
use lilia_core::{ErrorCode, LiliaError, Result};
use lilia_sqlite_native::{VTabValue, VirtualCursor};

pub type BatchIterator =
    Box<dyn Iterator<Item = std::result::Result<RecordBatch, arrow::error::ArrowError>> + Send>;

pub type ReaderFactory = Arc<dyn Fn() -> Result<BatchIterator> + Send + Sync>;

pub struct ArrowBatchCursor {
    _mmap: Arc<memmap2::Mmap>,
    factory: ReaderFactory,
    current_reader: Option<BatchIterator>,
    current_batch: Option<RecordBatch>,
    row_idx: usize,
    rowid: i64,
}

impl std::fmt::Debug for ArrowBatchCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArrowBatchCursor")
            .field("row_idx", &self.row_idx)
            .field("rowid", &self.rowid)
            .field("has_batch", &self.current_batch.is_some())
            .finish_non_exhaustive()
    }
}

impl ArrowBatchCursor {
    #[must_use]
    pub fn new(mmap: Arc<memmap2::Mmap>, factory: ReaderFactory) -> Self {
        Self {
            _mmap: mmap,
            factory,
            current_reader: None,
            current_batch: None,
            row_idx: 0,
            rowid: 0,
        }
    }

    fn advance_to_next_row(&mut self) -> Result<()> {
        if let Some(ref batch) = self.current_batch {
            if self.row_idx + 1 < batch.num_rows() {
                self.row_idx += 1;
                self.rowid += 1;
                return Ok(());
            }
        }

        // Current batch is exhausted or none; get next batch from reader
        while let Some(ref mut reader) = self.current_reader {
            match reader.next() {
                Some(Ok(batch)) => {
                    if batch.num_rows() > 0 {
                        self.current_batch = Some(batch);
                        self.row_idx = 0;
                        self.rowid += 1;
                        return Ok(());
                    }
                }
                Some(Err(err)) => {
                    return Err(LiliaError::new(
                        ErrorCode::Storage,
                        format!("arrow batch read error: {err}"),
                        false,
                    ));
                }
                None => {
                    // EOF
                    self.current_batch = None;
                    return Ok(());
                }
            }
        }

        self.current_batch = None;
        Ok(())
    }
}

impl VirtualCursor for ArrowBatchCursor {
    fn filter(&mut self, _idx_num: i32, _idx_str: Option<&str>, _args: &[VTabValue]) -> Result<()> {
        let reader = (self.factory)()?;
        self.current_reader = Some(reader);
        self.current_batch = None;
        self.row_idx = 0;
        self.rowid = 0;
        self.advance_to_next_row()
    }

    fn next(&mut self) -> Result<()> {
        self.advance_to_next_row()
    }

    fn eof(&self) -> bool {
        self.current_batch.is_none()
    }

    fn column(&self, col: i32) -> Result<VTabValue> {
        let Some(ref batch) = self.current_batch else {
            return Ok(VTabValue::Null);
        };
        if col < 0 || (col as usize) >= batch.num_columns() {
            return Ok(VTabValue::Null);
        }
        let array = batch.column(col as usize);
        Ok(array_value_to_vtab(array.as_ref(), self.row_idx))
    }

    fn rowid(&self) -> i64 {
        self.rowid
    }
}

fn array_value_to_vtab(array: &dyn Array, row_idx: usize) -> VTabValue {
    if array.is_null(row_idx) {
        return VTabValue::Null;
    }
    match array.data_type() {
        DataType::Boolean => {
            let b = array.as_boolean().value(row_idx);
            VTabValue::Int(if b { 1 } else { 0 })
        }
        DataType::Int8 => {
            let v = array.as_primitive::<Int8Type>().value(row_idx);
            VTabValue::Int(i64::from(v))
        }
        DataType::Int16 => {
            let v = array.as_primitive::<Int16Type>().value(row_idx);
            VTabValue::Int(i64::from(v))
        }
        DataType::Int32 => {
            let v = array.as_primitive::<Int32Type>().value(row_idx);
            VTabValue::Int(i64::from(v))
        }
        DataType::Int64 => {
            let v = array.as_primitive::<Int64Type>().value(row_idx);
            VTabValue::Int(v)
        }
        DataType::UInt8 => {
            let v = array.as_primitive::<UInt8Type>().value(row_idx);
            VTabValue::Int(i64::from(v))
        }
        DataType::UInt16 => {
            let v = array.as_primitive::<UInt16Type>().value(row_idx);
            VTabValue::Int(i64::from(v))
        }
        DataType::UInt32 => {
            let v = array.as_primitive::<UInt32Type>().value(row_idx);
            VTabValue::Int(i64::from(v))
        }
        DataType::UInt64 => {
            let v = array.as_primitive::<UInt64Type>().value(row_idx);
            VTabValue::Int(v as i64)
        }
        DataType::Float32 => {
            let v = array.as_primitive::<Float32Type>().value(row_idx);
            VTabValue::Float(f64::from(v))
        }
        DataType::Float64 => {
            let v = array.as_primitive::<Float64Type>().value(row_idx);
            VTabValue::Float(v)
        }
        DataType::Utf8 => {
            let s = array.as_string::<i32>().value(row_idx);
            VTabValue::Text(s.to_string())
        }
        DataType::LargeUtf8 => {
            let s = array.as_string::<i64>().value(row_idx);
            VTabValue::Text(s.to_string())
        }
        DataType::Utf8View => {
            let s = array.as_string_view().value(row_idx);
            VTabValue::Text(s.to_string())
        }
        DataType::Binary => {
            let b = array.as_binary::<i32>().value(row_idx);
            VTabValue::Blob(b.to_vec())
        }
        DataType::LargeBinary => {
            let b = array.as_binary::<i64>().value(row_idx);
            VTabValue::Blob(b.to_vec())
        }
        DataType::BinaryView => {
            let b = array.as_binary_view().value(row_idx);
            VTabValue::Blob(b.to_vec())
        }
        _ => {
            if let Ok(formatted) = arrow::util::display::array_value_to_string(array, row_idx) {
                VTabValue::Text(formatted)
            } else {
                VTabValue::Null
            }
        }
    }
}
