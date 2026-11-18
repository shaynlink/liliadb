//! `DataFusion` table provider backed by a [`ColumnarSource`].

use std::sync::Arc;

use async_trait::async_trait;
use datafusion::arrow::array::RecordBatch;
use datafusion::arrow::datatypes::{DataType, SchemaRef, TimeUnit};
use datafusion::catalog::memory::MemorySourceConfig;
use datafusion::catalog::{Session, TableProvider};
use datafusion::error::{DataFusionError, Result as DataFusionResult};
use datafusion::logical_expr::{
    BinaryExpr, Expr, Operator, TableProviderFilterPushDown, TableType,
};
use datafusion::physical_plan::ExecutionPlan;
use datafusion::scalar::ScalarValue;

use crate::segment::{CmpOp, ColumnPredicate, Scalar};
use crate::source::{ColumnarSource, ScanRequest};

/// Exposes a columnar table to `DataFusion`.
///
/// Every scan takes one consistent snapshot of the source, pushes the projection down
/// so only the referenced columns are read, and hands the rows to the engine as
/// fixed-size batches spread over the configured number of partitions.
#[derive(Debug)]
pub struct ColumnarTableProvider {
    source: Arc<dyn ColumnarSource>,
    schema: SchemaRef,
    batch_size: usize,
    target_partitions: usize,
}

impl ColumnarTableProvider {
    /// Wraps `source`. `batch_size` and `target_partitions` are clamped to at least `1`.
    #[must_use]
    pub fn new(
        source: Arc<dyn ColumnarSource>,
        batch_size: usize,
        target_partitions: usize,
    ) -> Self {
        let schema = source.schema();
        Self {
            source,
            schema,
            batch_size: batch_size.max(1),
            target_partitions: target_partitions.max(1),
        }
    }
}

#[async_trait]
impl TableProvider for ColumnarTableProvider {
    fn schema(&self) -> SchemaRef {
        Arc::clone(&self.schema)
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    async fn scan(
        &self,
        _state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        _limit: Option<usize>,
    ) -> DataFusionResult<Arc<dyn ExecutionPlan>> {
        let output: Vec<usize> = match projection {
            Some(fields) => fields.clone(),
            None => (0..self.schema.fields().len()).collect(),
        };
        // `SELECT count(*)` asks for no column at all; the key column is read so the
        // row count (after the delta overlay) is still exact, then projected away.
        let requested = if output.is_empty() {
            vec![0]
        } else {
            output.clone()
        };
        let predicates: Vec<ColumnPredicate> = filters
            .iter()
            .filter_map(|filter| to_predicate(filter, &self.schema))
            .collect();

        let source = Arc::clone(&self.source);
        let scan_fields = requested.clone();
        let snapshot = tokio::task::spawn_blocking(move || {
            source.scan(&ScanRequest {
                projection: &scan_fields,
                predicates: &predicates,
            })
        })
        .await
        .map_err(|e| DataFusionError::Execution(format!("columnar scan task failed: {e}")))?
        .map_err(|e| DataFusionError::External(Box::new(e)))?;

        let batches = if output.is_empty() {
            snapshot
                .batches
                .iter()
                .map(|batch| batch.project(&[]))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            snapshot.batches
        };
        let output_schema = Arc::new(self.schema.project(&output)?);
        let partitions = partition(&batches, self.batch_size, self.target_partitions);
        let plan = MemorySourceConfig::try_new_exec(&partitions, output_schema, None)?;
        Ok(plan)
    }

    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> DataFusionResult<Vec<TableProviderFilterPushDown>> {
        Ok(filters
            .iter()
            .map(|filter| {
                if to_predicate(filter, &self.schema).is_some() {
                    // Zone maps only skip segments; rows are still filtered by the engine.
                    TableProviderFilterPushDown::Inexact
                } else {
                    TableProviderFilterPushDown::Unsupported
                }
            })
            .collect())
    }
}

/// Cuts batches into zero-copy slices of at most `batch_size` rows and deals them
/// round-robin over at most `target_partitions` partitions.
fn partition(
    batches: &[RecordBatch],
    batch_size: usize,
    target_partitions: usize,
) -> Vec<Vec<RecordBatch>> {
    let mut slices = Vec::new();
    for batch in batches {
        let mut offset = 0;
        while offset < batch.num_rows() {
            let length = batch_size.min(batch.num_rows() - offset);
            slices.push(batch.slice(offset, length));
            offset += length;
        }
    }
    let partition_count = target_partitions.min(slices.len()).max(1);
    let mut partitions = vec![Vec::new(); partition_count];
    for (index, slice) in slices.into_iter().enumerate() {
        partitions[index % partition_count].push(slice);
    }
    partitions
}

/// Recognizes `column <op> literal` (in either order) on a column whose type the
/// literal matches, which is the only shape zone maps can evaluate.
fn to_predicate(filter: &Expr, schema: &SchemaRef) -> Option<ColumnPredicate> {
    let Expr::BinaryExpr(BinaryExpr { left, op, right }) = filter else {
        return None;
    };
    let (column, literal, op) = match (left.as_ref(), right.as_ref()) {
        (Expr::Column(column), Expr::Literal(literal, _)) => (column, literal, compare_op(*op)?),
        (Expr::Literal(literal, _), Expr::Column(column)) => {
            (column, literal, mirror(compare_op(*op)?))
        }
        _ => return None,
    };
    let field = schema.index_of(&column.name).ok()?;
    let value = to_scalar(literal, schema.field(field).data_type())?;
    Some(ColumnPredicate { field, op, value })
}

fn compare_op(op: Operator) -> Option<CmpOp> {
    match op {
        Operator::Eq => Some(CmpOp::Eq),
        Operator::Lt => Some(CmpOp::Lt),
        Operator::LtEq => Some(CmpOp::LtEq),
        Operator::Gt => Some(CmpOp::Gt),
        Operator::GtEq => Some(CmpOp::GtEq),
        _ => None,
    }
}

/// Rewrites the operator of `literal <op> column` as `column <op'> literal`.
fn mirror(op: CmpOp) -> CmpOp {
    match op {
        CmpOp::Eq => CmpOp::Eq,
        CmpOp::Lt => CmpOp::Gt,
        CmpOp::LtEq => CmpOp::GtEq,
        CmpOp::Gt => CmpOp::Lt,
        CmpOp::GtEq => CmpOp::LtEq,
    }
}

fn to_scalar(literal: &ScalarValue, column_type: &DataType) -> Option<Scalar> {
    match column_type {
        DataType::Int64 | DataType::Float64 => match literal {
            ScalarValue::Int8(Some(v)) => Some(Scalar::Int(i64::from(*v))),
            ScalarValue::Int16(Some(v)) => Some(Scalar::Int(i64::from(*v))),
            ScalarValue::Int32(Some(v)) => Some(Scalar::Int(i64::from(*v))),
            ScalarValue::Int64(Some(v)) => Some(Scalar::Int(*v)),
            ScalarValue::UInt8(Some(v)) => Some(Scalar::Int(i64::from(*v))),
            ScalarValue::UInt16(Some(v)) => Some(Scalar::Int(i64::from(*v))),
            ScalarValue::UInt32(Some(v)) => Some(Scalar::Int(i64::from(*v))),
            ScalarValue::UInt64(Some(v)) => i64::try_from(*v).ok().map(Scalar::Int),
            ScalarValue::Float32(Some(v)) => Some(Scalar::Float(f64::from(*v))),
            ScalarValue::Float64(Some(v)) => Some(Scalar::Float(*v)),
            _ => None,
        },
        DataType::Boolean => match literal {
            ScalarValue::Boolean(Some(v)) => Some(Scalar::Bool(*v)),
            _ => None,
        },
        DataType::Utf8 => match literal {
            ScalarValue::Utf8(Some(v))
            | ScalarValue::LargeUtf8(Some(v))
            | ScalarValue::Utf8View(Some(v)) => Some(Scalar::Text(v.clone())),
            _ => None,
        },
        DataType::Timestamp(TimeUnit::Millisecond, _) => match literal {
            ScalarValue::TimestampMillisecond(Some(v), _) => Some(Scalar::Int(*v)),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use datafusion::arrow::array::Int64Array;
    use datafusion::arrow::datatypes::{Field, Schema};
    use datafusion::logical_expr::{col, lit};

    use super::*;

    fn schema() -> SchemaRef {
        Arc::new(Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new("age", DataType::Int64, true),
            Field::new(
                "seen_at",
                DataType::Timestamp(TimeUnit::Millisecond, None),
                true,
            ),
        ]))
    }

    #[test]
    fn recognizes_column_literal_comparisons_in_both_orders() {
        let schema = schema();
        assert_eq!(
            to_predicate(&col("age").gt(lit(30_i64)), &schema),
            Some(ColumnPredicate {
                field: 1,
                op: CmpOp::Gt,
                value: Scalar::Int(30)
            })
        );
        assert_eq!(
            to_predicate(&lit(30.5_f64).gt_eq(col("age")), &schema),
            Some(ColumnPredicate {
                field: 1,
                op: CmpOp::LtEq,
                value: Scalar::Float(30.5)
            })
        );
        assert_eq!(
            to_predicate(&col("id").eq(lit("u1")), &schema),
            Some(ColumnPredicate {
                field: 0,
                op: CmpOp::Eq,
                value: Scalar::Text("u1".into())
            })
        );
    }

    #[test]
    fn ignores_shapes_zone_maps_cannot_evaluate() {
        let schema = schema();
        // Mismatched literal type, unsupported operator, expression operand, unknown column.
        assert_eq!(to_predicate(&col("age").eq(lit("thirty")), &schema), None);
        assert_eq!(to_predicate(&col("seen_at").gt(lit(5_i64)), &schema), None);
        assert_eq!(to_predicate(&col("age").not_eq(lit(1_i64)), &schema), None);
        assert_eq!(
            to_predicate(&(col("age") + lit(1_i64)).gt(lit(2_i64)), &schema),
            None
        );
        assert_eq!(to_predicate(&col("missing").eq(lit(1_i64)), &schema), None);
        assert_eq!(to_predicate(&col("age").is_null(), &schema), None);
    }

    #[test]
    fn partitions_are_sliced_and_balanced() {
        let schema = Arc::new(Schema::new(vec![Field::new("v", DataType::Int64, false)]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(Int64Array::from((0..2500).collect::<Vec<i64>>()))],
        )
        .unwrap();

        let partitions = partition(std::slice::from_ref(&batch), 1024, 2);
        assert_eq!(partitions.len(), 2);
        let sizes: Vec<Vec<usize>> = partitions
            .iter()
            .map(|p| p.iter().map(RecordBatch::num_rows).collect())
            .collect();
        assert_eq!(sizes, vec![vec![1024, 452], vec![1024]]);

        // Fewer slices than partitions, and no rows at all.
        assert_eq!(partition(&[batch], 4096, 8).len(), 1);
        assert_eq!(partition(&[], 1024, 4), vec![Vec::<RecordBatch>::new()]);
    }
}
