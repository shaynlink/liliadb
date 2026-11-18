use arrow::datatypes::{DataType, Schema};

/// Map an Arrow data type to an affinity in SQLite.
#[must_use]
pub fn arrow_type_to_sqlite(dt: &DataType) -> &'static str {
    match dt {
        DataType::Boolean
        | DataType::Int8
        | DataType::Int16
        | DataType::Int32
        | DataType::Int64
        | DataType::UInt8
        | DataType::UInt16
        | DataType::UInt32
        | DataType::UInt64 => "INTEGER",

        DataType::Float16 | DataType::Float32 | DataType::Float64 => "REAL",

        DataType::Binary
        | DataType::LargeBinary
        | DataType::BinaryView
        | DataType::FixedSizeBinary(_) => "BLOB",

        _ => "TEXT",
    }
}

/// Generate a SQLite CREATE TABLE DDL string from an Arrow schema.
#[must_use]
pub fn build_sqlite_ddl(table_name: &str, schema: &Schema) -> String {
    let mut cols = Vec::with_capacity(schema.fields().len());
    for field in schema.fields() {
        let name = field.name().replace('"', "\"\"");
        let sqlite_type = arrow_type_to_sqlite(field.data_type());
        cols.push(format!("\"{name}\" {sqlite_type}"));
    }
    let sanitized_table = table_name.replace('"', "\"\"");
    format!("CREATE TABLE \"{sanitized_table}\" ({});", cols.join(", "))
}
