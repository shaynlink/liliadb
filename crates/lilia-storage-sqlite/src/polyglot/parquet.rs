use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::datatypes::SchemaRef;
use bytes::Bytes;
use lilia_core::{ErrorCode, LiliaError, Result};
use lilia_sqlite_native::{
    IndexConstraint, IndexPlan, VirtualCursor, VirtualTableFactory, VirtualTableProvider,
};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::file::reader::{ChunkReader, Length};

use super::cursor::ArrowBatchCursor;
use super::options::{parse_parquet_args, ParquetOptions};
use super::schema::build_sqlite_ddl;

#[derive(Clone)]
pub struct MmapChunkReader(pub Arc<memmap2::Mmap>);

impl std::fmt::Debug for MmapChunkReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MmapChunkReader")
            .field("len", &self.0.len())
            .finish()
    }
}

impl Length for MmapChunkReader {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }
}

impl ChunkReader for MmapChunkReader {
    type T = std::io::Cursor<Bytes>;

    fn get_read(&self, start: u64) -> parquet::errors::Result<Self::T> {
        let remaining = (self.0.len() as u64).saturating_sub(start);
        let bytes = self.get_bytes(start, remaining as usize)?;
        Ok(std::io::Cursor::new(bytes))
    }

    fn get_bytes(&self, start: u64, length: usize) -> parquet::errors::Result<Bytes> {
        let start_usize = start as usize;
        let end_usize = start_usize.saturating_add(length);
        if end_usize > self.0.len() {
            return Err(parquet::errors::ParquetError::General(format!(
                "parquet chunk read out of bounds: start={start}, length={length}, file_size={}",
                self.0.len()
            )));
        }
        Ok(Bytes::copy_from_slice(&self.0[start_usize..end_usize]))
    }
}

pub struct ParquetTableProvider {
    pub path: PathBuf,
    pub mmap: Arc<memmap2::Mmap>,
    pub schema: SchemaRef,
    pub ddl: String,
    pub options: ParquetOptions,
    pub num_rows: usize,
}

impl std::fmt::Debug for ParquetTableProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParquetTableProvider")
            .field("path", &self.path)
            .field("schema", &self.schema)
            .field("num_rows", &self.num_rows)
            .finish_non_exhaustive()
    }
}

impl ParquetTableProvider {
    pub fn open(path: impl AsRef<Path>, table_name: &str, options: ParquetOptions) -> Result<Self> {
        let path = path.as_ref();
        let file = File::open(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                LiliaError::new(
                    ErrorCode::NotFound,
                    format!("parquet file not found: {}", path.display()),
                    false,
                )
            } else {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("failed to open parquet file {}: {e}", path.display()),
                    false,
                )
            }
        })?;

        let mmap = unsafe {
            memmap2::Mmap::map(&file).map_err(|e| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("failed to mmap parquet file {}: {e}", path.display()),
                    false,
                )
            })?
        };
        let mmap = Arc::new(mmap);

        let builder = ParquetRecordBatchReaderBuilder::try_new(MmapChunkReader(Arc::clone(&mmap)))
            .map_err(|e| {
                LiliaError::new(
                    ErrorCode::Storage,
                    format!("invalid parquet file {}: {e}", path.display()),
                    false,
                )
            })?;

        let schema = builder.schema().clone();
        let num_rows = builder.metadata().file_metadata().num_rows() as usize;
        let ddl = build_sqlite_ddl(table_name, &schema);

        Ok(Self {
            path: path.to_path_buf(),
            mmap,
            schema,
            ddl,
            options,
            num_rows,
        })
    }
}

impl VirtualTableProvider for ParquetTableProvider {
    fn schema_ddl(&self) -> &str {
        &self.ddl
    }

    fn best_index(&self, _constraints: &[IndexConstraint]) -> IndexPlan {
        IndexPlan {
            idx_num: 0,
            idx_str: None,
            constraint_usage: Vec::new(),
            estimated_cost: (self.num_rows as f64) * 0.05,
            estimated_rows: self.num_rows as i64,
            unique_scan: false,
        }
    }

    fn open_cursor(&self) -> Result<Box<dyn VirtualCursor>> {
        let mmap = Arc::clone(&self.mmap);
        let batch_size = self.options.batch_size;
        let factory = Arc::new(move || {
            let reader =
                ParquetRecordBatchReaderBuilder::try_new(MmapChunkReader(Arc::clone(&mmap)))
                    .map_err(|e| {
                        LiliaError::new(
                            ErrorCode::Storage,
                            format!("failed to instantiate parquet reader: {e}"),
                            false,
                        )
                    })?
                    .with_batch_size(batch_size)
                    .build()
                    .map_err(|e| {
                        LiliaError::new(
                            ErrorCode::Storage,
                            format!("failed to build parquet batch reader: {e}"),
                            false,
                        )
                    })?;
            Ok(Box::new(reader) as super::cursor::BatchIterator)
        });

        Ok(Box::new(ArrowBatchCursor::new(
            Arc::clone(&self.mmap),
            factory,
        )))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ParquetFactory;

impl VirtualTableFactory for ParquetFactory {
    fn create(&self, table_name: &str, args: &[&str]) -> Result<Arc<dyn VirtualTableProvider>> {
        let (path_str, options) = parse_parquet_args(args);
        if path_str.is_empty() {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "read_parquet requires a file path argument",
                false,
            ));
        }
        let provider = ParquetTableProvider::open(Path::new(&path_str), table_name, options)?;
        Ok(Arc::new(provider))
    }
}
