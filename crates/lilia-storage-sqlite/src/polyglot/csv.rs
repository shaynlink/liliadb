use std::fs::File;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::csv::reader::Format;
use arrow::csv::ReaderBuilder;
use arrow::datatypes::SchemaRef;
use lilia_core::{ErrorCode, LiliaError, Result};
use lilia_sqlite_native::{
    IndexConstraint, IndexPlan, VirtualCursor, VirtualTableFactory, VirtualTableProvider,
};

use super::cursor::ArrowBatchCursor;
use super::options::{parse_csv_args, CsvOptions};
use super::schema::build_sqlite_ddl;

#[derive(Clone)]
struct MmapSlice(Arc<memmap2::Mmap>);

impl std::fmt::Debug for MmapSlice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MmapSlice")
            .field("len", &self.0.len())
            .finish()
    }
}

impl AsRef<[u8]> for MmapSlice {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

pub struct CsvTableProvider {
    pub path: PathBuf,
    pub mmap: Arc<memmap2::Mmap>,
    pub schema: SchemaRef,
    pub ddl: String,
    pub options: CsvOptions,
}

impl std::fmt::Debug for CsvTableProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CsvTableProvider")
            .field("path", &self.path)
            .field("schema", &self.schema)
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

impl CsvTableProvider {
    pub fn open(path: impl AsRef<Path>, table_name: &str, options: CsvOptions) -> Result<Self> {
        let path = path.as_ref();
        let file = File::open(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                LiliaError::new(
                    ErrorCode::NotFound,
                    format!("csv file not found: {}", path.display()),
                    false,
                )
            } else {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("failed to open csv file {}: {e}", path.display()),
                    false,
                )
            }
        })?;

        let mmap = unsafe {
            memmap2::Mmap::map(&file).map_err(|e| {
                LiliaError::new(
                    ErrorCode::Io,
                    format!("failed to mmap csv file {}: {e}", path.display()),
                    false,
                )
            })?
        };
        let mmap = Arc::new(mmap);

        let format = Format::default()
            .with_delimiter(options.delimiter)
            .with_header(options.has_header);

        let cursor = Cursor::new(&mmap[..]);
        let (schema, _records_inferred) = format.infer_schema(cursor, Some(100)).map_err(|e| {
            LiliaError::new(
                ErrorCode::Storage,
                format!("failed to infer csv schema {}: {e}", path.display()),
                false,
            )
        })?;

        let schema = Arc::new(schema);
        let ddl = build_sqlite_ddl(table_name, &schema);

        Ok(Self {
            path: path.to_path_buf(),
            mmap,
            schema,
            ddl,
            options,
        })
    }
}

impl VirtualTableProvider for CsvTableProvider {
    fn schema_ddl(&self) -> &str {
        &self.ddl
    }

    fn best_index(&self, _constraints: &[IndexConstraint]) -> IndexPlan {
        IndexPlan {
            idx_num: 0,
            idx_str: None,
            constraint_usage: Vec::new(),
            estimated_cost: 1000.0,
            estimated_rows: 1000,
            unique_scan: false,
        }
    }

    fn open_cursor(&self) -> Result<Box<dyn VirtualCursor>> {
        let mmap = Arc::clone(&self.mmap);
        let schema = Arc::clone(&self.schema);
        let options = self.options.clone();

        let factory = Arc::new(move || {
            let cursor = Cursor::new(MmapSlice(Arc::clone(&mmap)));
            let reader = ReaderBuilder::new(Arc::clone(&schema))
                .with_header(options.has_header)
                .with_delimiter(options.delimiter)
                .with_quote(options.quote)
                .with_batch_size(2048)
                .build(cursor)
                .map_err(|e| {
                    LiliaError::new(
                        ErrorCode::Storage,
                        format!("failed to build csv reader: {e}"),
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
pub struct CsvFactory;

impl VirtualTableFactory for CsvFactory {
    fn create(&self, table_name: &str, args: &[&str]) -> Result<Arc<dyn VirtualTableProvider>> {
        let (path_str, options) = parse_csv_args(args);
        if path_str.is_empty() {
            return Err(LiliaError::new(
                ErrorCode::InvalidInput,
                "read_csv requires a file path argument",
                false,
            ));
        }
        let provider = CsvTableProvider::open(Path::new(&path_str), table_name, options)?;
        Ok(Arc::new(provider))
    }
}
