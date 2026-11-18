use std::collections::HashSet;
use std::sync::Arc;

use datafusion::arrow::datatypes::{DataType, Field, Schema, SchemaRef, TimeUnit};
use serde::{Deserialize, Serialize};

use crate::error::{AnalyticsError, Result};

/// Logical type of a projected column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnType {
    Int64,
    Float64,
    Boolean,
    Utf8,
    /// Milliseconds since the Unix epoch, without a time zone.
    TimestampMillis,
}

impl ColumnType {
    /// Arrow physical type backing this logical type.
    #[must_use]
    pub fn data_type(self) -> DataType {
        match self {
            Self::Int64 => DataType::Int64,
            Self::Float64 => DataType::Float64,
            Self::Boolean => DataType::Boolean,
            Self::Utf8 => DataType::Utf8,
            Self::TimestampMillis => DataType::Timestamp(TimeUnit::Millisecond, None),
        }
    }
}

/// One projected column: where its value comes from and how it is typed.
///
/// `path` is a JSON path (`$.address.city`) for a JSON collection, or a column name
/// for a SQL table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnSpec {
    pub name: String,
    pub path: String,
    pub ty: ColumnType,
}

impl ColumnSpec {
    #[must_use]
    pub fn new(name: impl Into<String>, path: impl Into<String>, ty: ColumnType) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            ty,
        }
    }
}

/// Type of the row key that ties a columnar row back to its row-store origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyKind {
    /// Document id of a JSON collection, exposed as the `id` column.
    Text,
    /// `rowid` of a SQL table, exposed as the `rowid` column.
    Integer,
}

impl KeyKind {
    /// Name under which the key is exposed to queries.
    #[must_use]
    pub fn column_name(self) -> &'static str {
        match self {
            Self::Text => "id",
            Self::Integer => "rowid",
        }
    }

    /// Arrow physical type of the key column.
    #[must_use]
    pub fn data_type(self) -> DataType {
        match self {
            Self::Text => DataType::Utf8,
            Self::Integer => DataType::Int64,
        }
    }
}

/// Schema of a columnar table: the key column at field `0`, then the projected columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSchema {
    key: KeyKind,
    columns: Vec<ColumnSpec>,
    arrow: SchemaRef,
}

impl TableSchema {
    /// Builds and validates a table schema.
    ///
    /// # Errors
    /// Returns [`AnalyticsError::InvalidInput`] when there is no column, when a column
    /// name is not a lowercase identifier, is duplicated, or collides with the key column.
    pub fn new(key: KeyKind, columns: Vec<ColumnSpec>) -> Result<Self> {
        if columns.is_empty() {
            return Err(AnalyticsError::InvalidInput(
                "a columnar table needs at least one column".into(),
            ));
        }
        let mut seen = HashSet::with_capacity(columns.len() + 1);
        seen.insert(key.column_name());
        let mut fields = Vec::with_capacity(columns.len() + 1);
        fields.push(Field::new(key.column_name(), key.data_type(), false));
        for column in &columns {
            validate_identifier("column name", &column.name)?;
            if column.path.is_empty() {
                return Err(AnalyticsError::InvalidInput(format!(
                    "column '{}' has an empty source path",
                    column.name
                )));
            }
            if !seen.insert(column.name.as_str()) {
                return Err(AnalyticsError::InvalidInput(format!(
                    "column name '{}' is duplicated or reserved for the key column",
                    column.name
                )));
            }
            fields.push(Field::new(&column.name, column.ty.data_type(), true));
        }
        Ok(Self {
            key,
            columns,
            arrow: Arc::new(Schema::new(fields)),
        })
    }

    #[must_use]
    pub fn key(&self) -> KeyKind {
        self.key
    }

    #[must_use]
    pub fn columns(&self) -> &[ColumnSpec] {
        &self.columns
    }

    /// Arrow schema, key column first.
    #[must_use]
    pub fn arrow(&self) -> SchemaRef {
        Arc::clone(&self.arrow)
    }

    /// Number of Arrow fields: the key column plus every projected column.
    #[must_use]
    pub fn field_count(&self) -> usize {
        self.columns.len() + 1
    }
}

/// Checks that `value` is a lowercase SQL identifier (`[a-z_][a-z0-9_]*`, at most 63 bytes).
///
/// `DataFusion` folds unquoted identifiers to lowercase, so restricting names to this
/// alphabet guarantees a table or column is always reachable without quoting.
///
/// # Errors
/// Returns [`AnalyticsError::InvalidInput`] naming `kind` when the identifier is rejected.
pub fn validate_identifier(kind: &str, value: &str) -> Result<()> {
    let mut chars = value.chars();
    let starts_well = chars
        .next()
        .is_some_and(|first| first.is_ascii_lowercase() || first == '_');
    let continues_well = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if starts_well && continues_well && value.len() <= 63 {
        Ok(())
    } else {
        Err(AnalyticsError::InvalidInput(format!(
            "{kind} '{value}' must match [a-z_][a-z0-9_]* and be at most 63 characters"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_places_the_key_first_and_makes_columns_nullable() {
        let schema = TableSchema::new(
            KeyKind::Text,
            vec![
                ColumnSpec::new("age", "$.age", ColumnType::Int64),
                ColumnSpec::new("seen_at", "$.seen", ColumnType::TimestampMillis),
            ],
        )
        .unwrap();
        let arrow = schema.arrow();
        assert_eq!(schema.field_count(), 3);
        assert_eq!(arrow.field(0).name(), "id");
        assert!(!arrow.field(0).is_nullable());
        assert_eq!(arrow.field(1).data_type(), &DataType::Int64);
        assert!(arrow.field(2).is_nullable());
    }

    #[test]
    fn schema_rejects_bad_or_colliding_names() {
        let column = |name: &str| ColumnSpec::new(name, "$.x", ColumnType::Int64);
        assert!(TableSchema::new(KeyKind::Text, Vec::new()).is_err());
        assert!(TableSchema::new(KeyKind::Text, vec![column("Age")]).is_err());
        assert!(TableSchema::new(KeyKind::Text, vec![column("id")]).is_err());
        assert!(TableSchema::new(KeyKind::Integer, vec![column("rowid")]).is_err());
        assert!(TableSchema::new(KeyKind::Integer, vec![column("id")]).is_ok());
        assert!(TableSchema::new(KeyKind::Text, vec![column("a"), column("a")]).is_err());
        assert!(TableSchema::new(
            KeyKind::Text,
            vec![ColumnSpec::new("a", "", ColumnType::Int64)]
        )
        .is_err());
    }

    #[test]
    fn identifiers_are_lowercase_and_bounded() {
        assert!(validate_identifier("table name", "users_col2").is_ok());
        assert!(validate_identifier("table name", "_t").is_ok());
        assert!(validate_identifier("table name", "").is_err());
        assert!(validate_identifier("table name", "2fast").is_err());
        assert!(validate_identifier("table name", "with space").is_err());
        assert!(validate_identifier("table name", &"a".repeat(64)).is_err());
    }
}
