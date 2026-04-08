use std::collections::HashMap;
use std::future::Future;
use std::pin::pin;

use futures::future::BoxFuture;
use futures::{FutureExt, StreamExt};

use crate::binder::BoundAlterAction;
use crate::{
    accessor::Accessor,
    binder::{BoundAssignment, BoundExpr, BoundJoinCondition, BoundJoinKind, FunctionKind},
    catalog,
    common::txn::Txn,
    databox::{TupleLayout, Value},
    error::{DbError, Result},
    optimizer::{
        PhysAggregateExpr, PhysicalPlan, PhysIndexScan, PhysInsert, PhysSeqScan, PhysSortKey,
        PhysValues,
    },
    planner::Schema,
};

use super::{
    evaluate::{evaluate_expr, evaluate_predicate},
    Tuple,
};

/// Result of executing a query.
#[derive(Debug)]
pub enum ExecutionResult {
    /// Query returned rows.
    Rows {
        columns: Vec<String>,
        rows: Vec<Tuple>,
    },
    /// DML/DDL returned affected row count.
    RowsAffected(u64),
    /// DDL completed successfully.
    Ok(String),
}

/// Execute a physical plan and return the result.
/// Uses BoxFuture for recursive calls to avoid infinite sized futures.
pub fn execute<'a, A: Accessor + 'a>(
    plan: PhysicalPlan,
    accessor: &'a A,
    txn: Txn,
) -> BoxFuture<'a, Result<ExecutionResult>> {
    async move {
        match plan {
            PhysicalPlan::Nothing => Ok(ExecutionResult::Ok("OK".to_string())),

            // DDL operations
            PhysicalPlan::CreateTable(ct) => {
                // Convert BoundColumnDefs to catalog::Columns
                let columns: Vec<catalog::Column> = ct
                    .columns
                    .iter()
                    .map(|c| catalog::Column {
                        oid: c.oid,
                        table_oid: ct.table_oid,
                        name: std::borrow::Cow::Owned(c.name.clone()),
                        type_id: c.data_type,
                        position: c.position,
                        nullable: c.nullable,
                        is_unique: c.unique,
                        is_primary_key: c.primary_key,
                    })
                    .collect();

                let table = catalog::Table {
                    oid: ct.table_oid,
                    name: std::borrow::Cow::Owned(ct.name.clone()),
                    file_id: ct.file_id,
                };

                accessor
                    .create_table(txn, table, columns)
                    .await
                    .map_err(|e| DbError::AccessorError(format!("{:?}", e)))?;

                Ok(ExecutionResult::Ok(format!("CREATE TABLE {}", ct.name)))
            }

            PhysicalPlan::DropTable(dt) => {
                // Table OID 0 means the table wasn't found but IF EXISTS was used, so we skip deleting
                if dt.table_oid != 0 {
                    accessor
                        .drop_table(txn, dt.table_oid, dt.name.clone())
                        .await
                        .map_err(|e| DbError::AccessorError(format!("{:?}", e)))?;
                }
                
                Ok(ExecutionResult::Ok(format!("DROP TABLE {}", dt.name)))
            }

            PhysicalPlan::AlterTable(at) => {
                match at.action {
                    BoundAlterAction::AddColumn(c) => {
                        let col = catalog::Column {
                            oid: c.oid,
                            table_oid: at.table_oid,
                            name: std::borrow::Cow::Owned(c.name.clone()),
                            type_id: c.data_type,
                            position: c.position,
                            nullable: c.nullable,
                            is_unique: c.unique,
                            is_primary_key: c.primary_key,
                        };
                        
                        accessor
                            .add_column(txn, at.table_oid, col)
                            .await
                            .map_err(|e| DbError::AccessorError(format!("{:?}", e)))?;
                            
                        Ok(ExecutionResult::Ok(format!("ALTER TABLE {} ADD COLUMN {}", at.name, c.name)))
                    }
                    BoundAlterAction::DropColumn { column_oid, .. } => {
                        let dropped_name = format!("__abdb_dropped_{}", column_oid);
                        accessor
                            .rename_column(txn, at.table_oid, column_oid, dropped_name)
                            .await
                            .map_err(|e| DbError::AccessorError(format!("{:?}", e)))?;
                        Ok(ExecutionResult::Ok(format!("ALTER TABLE {} DROP COLUMN", at.name)))
                    }
                    BoundAlterAction::RenameColumn { column_oid, new_name, .. } => {
                        accessor
                            .rename_column(txn, at.table_oid, column_oid, new_name.clone())
                            .await
                            .map_err(|e| DbError::AccessorError(format!("{:?}", e)))?;
                        Ok(ExecutionResult::Ok(format!("ALTER TABLE {} RENAME COLUMN TO {}", at.name, new_name)))
                    }
                    _ => Err(DbError::Unsupported("Only ADD COLUMN is currently implemented".to_string())),
                }
            }

            PhysicalPlan::CreateIndex(ci) => {
                let index = catalog::Index {
                    oid: ci.index_oid,
                    name: std::borrow::Cow::Owned(ci.name.clone()),
                    table_oid: ci.table_oid,
                    file_id: ci.file_id,
                    column_oid: ci.column_oid,
                };

                accessor
                    .create_index(txn, index.clone())
                    .await
                    .map_err(|e| DbError::AccessorError(format!("{:?}", e)))?;

                let table = accessor.catalog_get_table_by_oid(txn, ci.table_oid)
                    .map_err(|e| DbError::AccessorError(format!("{:?}", e)))?;
                let columns = accessor.catalog_get_table_columns(txn, ci.table_oid)
                    .map_err(|e| DbError::AccessorError(format!("{:?}", e)))?;
                
                let target_col_idx = columns.iter().position(|c| c.oid == ci.column_oid)
                    .ok_or_else(|| DbError::Internal("Indexed column not found".into()))?;

                let scan = PhysSeqScan {
                    table: table.clone(),
                    columns: columns.clone(),
                    alias: None,
                    pushed_predicates: vec![],
                    schema: Schema::empty(),
                };

                let rows = execute_seq_scan(&scan, accessor, txn).await?;
                for row in rows {
                    if let Some(val) = row.get(target_col_idx) {
                        if !val.is_null() {
                            let rid = row.rid.unwrap();
                            let key_bytes = val.to_bytes();
                            accessor.index_insert(txn, ci.index_oid, key_bytes, rid)
                                .await
                                .map_err(|e| DbError::AccessorError(format!("{:?}", e)))?;
                        }
                    }
                }

                Ok(ExecutionResult::Ok(format!("CREATE INDEX {}", ci.name)))
            }

            PhysicalPlan::DropIndex(di) => {
                if di.index_oid != 0 {
                    accessor
                        .drop_index(txn, di.index_oid, di.name.clone())
                        .await
                        .map_err(|e| DbError::AccessorError(format!("{:?}", e)))?;
                }
                Ok(ExecutionResult::Ok(format!("DROP INDEX {}", di.name)))
            }

            PhysicalPlan::DescribeTable(_table, columns) => {
                let out_cols = vec![
                    "column_name".to_string(), 
                    "type".to_string(), 
                    "nullable".to_string()
                ];
                
                let mut rows = Vec::new();
                for col in columns {
                    // Hide soft-dropped columns!
                    if col.name.starts_with("__abdb_dropped_") {
                        continue;
                    }
                    rows.push(Tuple::new(vec![
                        Value::String(col.name.to_string()),
                        Value::String(col.type_id.to_string()),
                        Value::Bool(col.nullable),
                    ]));
                }
                
                Ok(ExecutionResult::Rows { columns: out_cols, rows })
            }

            PhysicalPlan::ShowTables => {
                let tables = accessor
                    .catalog_get_all_tables(txn)
                    .map_err(|e| DbError::AccessorError(format!("{:?}", e)))?;

                let out_cols = vec!["table_name".to_string(), "oid".to_string()];
                let mut rows = Vec::new();

                for table in tables {
                    if table.oid > crate::common::constants::SYS_TABLE_INDEXES_OID {
                        rows.push(Tuple::new(vec![
                            Value::String(table.name.to_string()),
                            Value::U32(table.oid),
                        ]));
                    }
                }

                rows.sort_by(|a, b| {
                    let name_a = a.values[0].as_string().unwrap_or_default();
                    let name_b = b.values[0].as_string().unwrap_or_default();
                    name_a.cmp(&name_b)
                });

                Ok(ExecutionResult::Rows {
                    columns: out_cols,
                    rows,
                })
            }

            // Query operations
            PhysicalPlan::Values(values) => {
                let columns = values
                    .schema
                    .columns
                    .iter()
                    .map(|c| c.name.clone())
                    .collect();
                let rows = execute_values(&values)?;
                Ok(ExecutionResult::Rows { columns, rows })
            }

            PhysicalPlan::SeqScan(scan) => {
                let columns = scan
                    .schema
                    .columns
                    .iter()
                    .map(|c| c.name.clone())
                    .collect();
                let rows = execute_seq_scan(&scan, accessor, txn).await?;
                Ok(ExecutionResult::Rows { columns, rows })
            }

            PhysicalPlan::Filter(filter) => {
                let inner = execute(*filter.input, accessor, txn).await?;
                match inner {
                    ExecutionResult::Rows { columns, rows } => {
                        let filtered = execute_filter(&filter.predicate, rows)?;
                        Ok(ExecutionResult::Rows {
                            columns,
                            rows: filtered,
                        })
                    }
                    other => Ok(other),
                }
            }

            PhysicalPlan::Projection(proj) => {
                let inner = execute(*proj.input, accessor, txn).await?;
                match inner {
                    ExecutionResult::Rows { rows, .. } => {
                        let projected = execute_projection(&proj.exprs, rows)?;
                        let columns = proj.aliases;
                        Ok(ExecutionResult::Rows {
                            columns,
                            rows: projected,
                        })
                    }
                    other => Ok(other),
                }
            }

            PhysicalPlan::Sort(sort) => {
                let inner = execute(*sort.input, accessor, txn).await?;
                match inner {
                    ExecutionResult::Rows { columns, mut rows } => {
                        execute_sort(&mut rows, &sort.order_by)?;
                        Ok(ExecutionResult::Rows { columns, rows })
                    }
                    other => Ok(other),
                }
            }

            PhysicalPlan::Limit(limit) => {
                let inner = execute(*limit.input, accessor, txn).await?;
                match inner {
                    ExecutionResult::Rows { columns, rows } => {
                        let limited = execute_limit(rows, &limit.limit, &limit.offset)?;
                        Ok(ExecutionResult::Rows {
                            columns,
                            rows: limited,
                        })
                    }
                    other => Ok(other),
                }
            }

            PhysicalPlan::TopN(topn) => {
                let inner = execute(*topn.input, accessor, txn).await?;
                match inner {
                    ExecutionResult::Rows { columns, mut rows } => {
                        execute_sort(&mut rows, &topn.order_by)?;
                        let limited = execute_limit(rows, &Some(topn.limit), &topn.offset)?;
                        Ok(ExecutionResult::Rows {
                            columns,
                            rows: limited,
                        })
                    }
                    other => Ok(other),
                }
            }

            PhysicalPlan::Distinct(distinct) => {
                let inner = execute(*distinct.input, accessor, txn).await?;
                match inner {
                    ExecutionResult::Rows { columns, rows } => {
                        let distinct_rows = execute_distinct(rows);
                        Ok(ExecutionResult::Rows {
                            columns,
                            rows: distinct_rows,
                        })
                    }
                    other => Ok(other),
                }
            }

            PhysicalPlan::HashDistinct(hash_distinct) => {
                let inner = execute(*hash_distinct.input, accessor, txn).await?;
                match inner {
                    ExecutionResult::Rows { columns, rows } => {
                        let distinct_rows = execute_distinct(rows);
                        Ok(ExecutionResult::Rows {
                            columns,
                            rows: distinct_rows,
                        })
                    }
                    other => Ok(other),
                }
            }

            PhysicalPlan::NestedLoopJoin(join) => {
                let left_result = execute(*join.left, accessor, txn).await?;
                let right_result = execute(*join.right, accessor, txn).await?;

                match (left_result, right_result) {
                    (
                        ExecutionResult::Rows {
                            columns: left_cols,
                            rows: left_rows,
                        },
                        ExecutionResult::Rows {
                            columns: right_cols,
                            rows: right_rows,
                        },
                    ) => {
                        let mut columns = left_cols;
                        columns.extend(right_cols);
                        let rows = execute_nested_loop_join(
                            left_rows,
                            right_rows,
                            &join.condition,
                            &join.kind,
                        )?;
                        Ok(ExecutionResult::Rows { columns, rows })
                    }
                    _ => Err(DbError::Internal("Join requires row inputs".to_string())),
                }
            }

            PhysicalPlan::HashJoin(join) => {
                let left_result = execute(*join.left, accessor, txn).await?;
                let right_result = execute(*join.right, accessor, txn).await?;

                match (left_result, right_result) {
                    (
                        ExecutionResult::Rows {
                            columns: left_cols,
                            rows: left_rows,
                        },
                        ExecutionResult::Rows {
                            columns: right_cols,
                            rows: right_rows,
                        },
                    ) => {
                        let mut columns = left_cols;
                        columns.extend(right_cols);
                        let rows = execute_hash_join(
                            left_rows,
                            right_rows,
                            &join.left_keys,
                            &join.right_keys,
                            &join.residual,
                            &join.kind,
                        )?;
                        Ok(ExecutionResult::Rows { columns, rows })
                    }
                    _ => Err(DbError::Internal("Join requires row inputs".to_string())),
                }
            }

            PhysicalPlan::HashAggregate(agg) => {
                let inner = execute(*agg.input, accessor, txn).await?;
                match inner {
                    ExecutionResult::Rows { rows, .. } => {
                        let (columns, agg_rows) = execute_hash_aggregate(
                            rows,
                            &agg.group_by,
                            &agg.aggregates,
                            &agg.schema,
                        )?;
                        Ok(ExecutionResult::Rows {
                            columns,
                            rows: agg_rows,
                        })
                    }
                    other => Ok(other),
                }
            }

            PhysicalPlan::StreamAggregate(agg) => {
                let inner = execute(*agg.input, accessor, txn).await?;
                match inner {
                    ExecutionResult::Rows { rows, .. } => {
                        let (columns, agg_rows) = execute_hash_aggregate(
                            rows,
                            &agg.group_by,
                            &agg.aggregates,
                            &agg.schema,
                        )?;
                        Ok(ExecutionResult::Rows {
                            columns,
                            rows: agg_rows,
                        })
                    }
                    other => Ok(other),
                }
            }

            PhysicalPlan::Insert(insert) => {
                let table = insert.table.clone();
                let table_columns = insert.table_columns.clone();
                let target_columns = insert.target_columns.clone();
                let source_result = execute(*insert.source, accessor, txn).await?;
                match source_result {
                    ExecutionResult::Rows { rows, .. } => {
                        let count = execute_insert(
                            rows,
                            &table,
                            &table_columns,
                            &target_columns,
                            accessor,
                            txn,
                        )
                        .await?;
                        Ok(ExecutionResult::RowsAffected(count))
                    }
                    _ => Err(DbError::Internal(
                        "Insert source must produce rows".to_string(),
                    )),
                }
            }

            PhysicalPlan::Update(update) => {
                let table = update.table.clone();
                let table_columns = update.table_columns.clone();
                let assignments = update.assignments.clone();
                let source_result = execute(*update.input, accessor, txn).await?;
                match source_result {
                    ExecutionResult::Rows { rows, .. } => {
                        let count =
                            execute_update(rows, &table, &table_columns, &assignments, accessor, txn)
                                .await?;
                        Ok(ExecutionResult::RowsAffected(count))
                    }
                    _ => Err(DbError::Internal(
                        "Update source must produce rows".to_string(),
                    )),
                }
            }

            PhysicalPlan::Delete(delete) => {
                let table = delete.table.clone();
                let source_result = execute(*delete.input, accessor, txn).await?;
                match source_result {
                    ExecutionResult::Rows { rows, .. } => {
                        let count = execute_delete(rows, &table, accessor, txn).await?;
                        Ok(ExecutionResult::RowsAffected(count))
                    }
                    _ => Err(DbError::Internal(
                        "Delete source must produce rows".to_string(),
                    )),
                }
            }

            PhysicalPlan::IndexScan(scan) => {
                let columns = scan
                    .schema
                    .columns
                    .iter()
                    .map(|c| c.name.clone())
                    .collect();
                let rows = execute_index_scan(&scan, accessor, txn).await?;
                Ok(ExecutionResult::Rows { columns, rows })
            }
        }
    }
    .boxed()
}

// Helper functions for each operator

fn execute_values(values: &PhysValues) -> Result<Vec<Tuple>> {
    let empty_tuple = Tuple::empty();
    values
        .rows
        .iter()
        .map(|row| {
            let vals: Result<Vec<Value>> =
                row.iter().map(|e| evaluate_expr(e, &empty_tuple)).collect();
            vals.map(Tuple::new)
        })
        .collect()
}

fn execute_seq_scan<'a, A: Accessor>(
    scan: &'a PhysSeqScan,
    accessor: &'a A,
    txn: Txn,
) -> impl Future<Output = Result<Vec<Tuple>>> + 'a {
    async move {
        let layout = TupleLayout::from(scan.columns.clone());
        let stream = accessor
            .table_scan(txn, scan.table.oid)
            .await
            .map_err(|e| DbError::AccessorError(format!("{:?}", e)))?;

        let mut stream = pin!(stream);

        let mut rows = Vec::new();
        while let Some(result) = stream.next().await {
            let (tuple_bytes, rid) =
                result.map_err(|e| DbError::AccessorError(format!("{:?}", e)))?;

            // Deserialize tuple from bytes using layout
            let mut values = Vec::with_capacity(scan.columns.len());
            for (idx, col) in scan.columns.iter().enumerate() {
                let val = layout
                    .read_field(&col.name, idx, &tuple_bytes)
                    .unwrap_or(Value::Null);
                values.push(val);
            }
            // Store RID for Update/Delete operations
            rows.push(Tuple::with_rid(values, rid));
        }

        // Apply pushed predicates if any
        if !scan.pushed_predicates.is_empty() {
            rows = rows
                .into_iter()
                .filter(|tuple| {
                    scan.pushed_predicates
                        .iter()
                        .all(|pred| evaluate_predicate(pred, tuple).unwrap_or(false))
                })
                .collect();
        }

        Ok(rows)
    }
}

fn execute_index_scan<'a, A: Accessor>(
    scan: &'a PhysIndexScan,
    accessor: &'a A,
    txn: Txn,
) -> impl Future<Output = Result<Vec<Tuple>>> + 'a {
    async move {
        let layout = TupleLayout::from(scan.columns.clone());
        let empty_tuple = Tuple::empty();

        // Evaluate start/end keys
        let start_key = match &scan.start_key {
            Some(expr) => {
                let val = evaluate_expr(expr, &empty_tuple)?;
                Some(val.to_bytes())
            }
            None => None,
        };

        let end_key = match &scan.end_key {
            Some(expr) => {
                let val = evaluate_expr(expr, &empty_tuple)?;
                Some(val.to_bytes())
            }
            None => None,
        };

        // Scan the index
        let stream = accessor
            .index_scan(txn, scan.index.oid, start_key, end_key)
            .await
            .map_err(|e| DbError::AccessorError(format!("{:?}", e)))?;

        let mut stream = pin!(stream);
        let mut rows = Vec::new();

        while let Some(result) = stream.next().await {
            let (_key_bytes, rid) =
                result.map_err(|e| DbError::AccessorError(format!("{:?}", e)))?;

            // Fetch the tuple from the table using the RID
            let tuple_bytes = accessor
                .table_get(txn, scan.table.oid, rid)
                .await
                .map_err(|e| DbError::AccessorError(format!("{:?}", e)))?;

            // Deserialize tuple from bytes using layout
            let mut values = Vec::with_capacity(scan.columns.len());
            for (idx, col) in scan.columns.iter().enumerate() {
                let val = layout
                    .read_field(&col.name, idx, &tuple_bytes)
                    .unwrap_or(Value::Null);
                values.push(val);
            }
            rows.push(Tuple::with_rid(values, rid));
        }

        // Apply residual predicates if any
        if !scan.residual_predicates.is_empty() {
            rows = rows
                .into_iter()
                .filter(|tuple| {
                    scan.residual_predicates
                        .iter()
                        .all(|pred| evaluate_predicate(pred, tuple).unwrap_or(false))
                })
                .collect();
        }

        Ok(rows)
    }
}

fn execute_filter(predicate: &BoundExpr, rows: Vec<Tuple>) -> Result<Vec<Tuple>> {
    rows.into_iter()
        .filter(|tuple| evaluate_predicate(predicate, tuple).unwrap_or(false))
        .map(Ok)
        .collect()
}

fn execute_projection(exprs: &[BoundExpr], rows: Vec<Tuple>) -> Result<Vec<Tuple>> {
    rows.into_iter()
        .map(|tuple| {
            let values: Result<Vec<Value>> =
                exprs.iter().map(|e| evaluate_expr(e, &tuple)).collect();
            values.map(Tuple::new)
        })
        .collect()
}

fn execute_sort(rows: &mut [Tuple], order_by: &[PhysSortKey]) -> Result<()> {
    rows.sort_by(|a, b| {
        for key in order_by {
            let av = evaluate_expr(&key.expr, a).unwrap_or(Value::Null);
            let bv = evaluate_expr(&key.expr, b).unwrap_or(Value::Null);

            let cmp = av.partial_cmp(&bv).unwrap_or(std::cmp::Ordering::Equal);

            let cmp = if key.asc { cmp } else { cmp.reverse() };

            // Handle nulls_first
            let cmp = match (av.is_null(), bv.is_null(), key.nulls_first) {
                (true, false, Some(true)) => std::cmp::Ordering::Less,
                (true, false, Some(false)) => std::cmp::Ordering::Greater,
                (false, true, Some(true)) => std::cmp::Ordering::Greater,
                (false, true, Some(false)) => std::cmp::Ordering::Less,
                _ => cmp,
            };

            if cmp != std::cmp::Ordering::Equal {
                return cmp;
            }
        }
        std::cmp::Ordering::Equal
    });
    Ok(())
}

fn execute_limit(
    rows: Vec<Tuple>,
    limit: &Option<BoundExpr>,
    offset: &Option<BoundExpr>,
) -> Result<Vec<Tuple>> {
    let empty = Tuple::empty();

    let skip = match offset {
        Some(e) => match evaluate_expr(e, &empty)? {
            Value::I64(n) => n.max(0) as usize,
            _ => 0,
        },
        None => 0,
    };

    let take = match limit {
        Some(e) => match evaluate_expr(e, &empty)? {
            Value::I64(n) => Some(n.max(0) as usize),
            _ => None,
        },
        None => None,
    };

    let iter = rows.into_iter().skip(skip);
    let result: Vec<Tuple> = match take {
        Some(n) => iter.take(n).collect(),
        None => iter.collect(),
    };

    Ok(result)
}

fn execute_distinct(rows: Vec<Tuple>) -> Vec<Tuple> {
    let mut seen: Vec<Tuple> = Vec::new();
    for row in rows {
        if !seen.contains(&row) {
            seen.push(row);
        }
    }
    seen
}

fn execute_nested_loop_join(
    left: Vec<Tuple>,
    right: Vec<Tuple>,
    condition: &BoundJoinCondition,
    kind: &BoundJoinKind,
) -> Result<Vec<Tuple>> {
    let mut result = Vec::new();

    match kind {
        BoundJoinKind::Inner | BoundJoinKind::Cross => {
            for l in &left {
                for r in &right {
                    let combined = l.concat(r);
                    if matches_join_condition(&combined, condition)? {
                        result.push(combined);
                    }
                }
            }
        }
        BoundJoinKind::LeftOuter => {
            for l in &left {
                let mut matched = false;
                for r in &right {
                    let combined = l.concat(r);
                    if matches_join_condition(&combined, condition)? {
                        result.push(combined);
                        matched = true;
                    }
                }
                if !matched {
                    let null_right =
                        Tuple::new(vec![Value::Null; right.first().map(|r| r.len()).unwrap_or(0)]);
                    result.push(l.concat(&null_right));
                }
            }
        }
        BoundJoinKind::RightOuter => {
            for r in &right {
                let mut matched = false;
                for l in &left {
                    let combined = l.concat(r);
                    if matches_join_condition(&combined, condition)? {
                        result.push(combined);
                        matched = true;
                    }
                }
                if !matched {
                    let null_left =
                        Tuple::new(vec![Value::Null; left.first().map(|l| l.len()).unwrap_or(0)]);
                    result.push(null_left.concat(r));
                }
            }
        }
        BoundJoinKind::FullOuter => {
            let mut right_matched = vec![false; right.len()];

            for l in &left {
                let mut matched = false;
                for (ri, r) in right.iter().enumerate() {
                    let combined = l.concat(r);
                    if matches_join_condition(&combined, condition)? {
                        result.push(combined);
                        matched = true;
                        right_matched[ri] = true;
                    }
                }
                if !matched {
                    let null_right =
                        Tuple::new(vec![Value::Null; right.first().map(|r| r.len()).unwrap_or(0)]);
                    result.push(l.concat(&null_right));
                }
            }

            // Add unmatched right rows
            for (ri, r) in right.iter().enumerate() {
                if !right_matched[ri] {
                    let null_left =
                        Tuple::new(vec![Value::Null; left.first().map(|l| l.len()).unwrap_or(0)]);
                    result.push(null_left.concat(r));
                }
            }
        }
    }

    Ok(result)
}

fn matches_join_condition(combined: &Tuple, condition: &BoundJoinCondition) -> Result<bool> {
    match condition {
        BoundJoinCondition::None => Ok(true),
        BoundJoinCondition::On(expr) => evaluate_predicate(expr, combined),
        BoundJoinCondition::Using(exprs) | BoundJoinCondition::Natural(exprs) => {
            for expr in exprs {
                if !evaluate_predicate(expr, combined)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
    }
}

/// Convert values to a hashable string key.
fn values_to_key(values: &[Value]) -> String {
    values
        .iter()
        .map(|v| format!("{:?}", v))
        .collect::<Vec<_>>()
        .join("|")
}

fn execute_hash_join(
    left: Vec<Tuple>,
    right: Vec<Tuple>,
    left_keys: &[BoundExpr],
    right_keys: &[BoundExpr],
    residual: &Option<BoundExpr>,
    kind: &BoundJoinKind,
) -> Result<Vec<Tuple>> {
    // Build hash table from left side using string keys (since Value can't be hashed)
    let mut hash_table: HashMap<String, Vec<Tuple>> = HashMap::new();
    for l in &left {
        let key_vals: Vec<Value> = left_keys
            .iter()
            .map(|k| evaluate_expr(k, l))
            .collect::<Result<_>>()?;
        let key = values_to_key(&key_vals);
        hash_table.entry(key).or_default().push(l.clone());
    }

    let mut result = Vec::new();

    match kind {
        BoundJoinKind::Inner | BoundJoinKind::Cross => {
            for r in &right {
                let key_vals: Vec<Value> = right_keys
                    .iter()
                    .map(|k| evaluate_expr(k, r))
                    .collect::<Result<_>>()?;
                let key = values_to_key(&key_vals);
                if let Some(matches) = hash_table.get(&key) {
                    for l in matches {
                        let combined = l.concat(r);
                        if check_residual(&combined, residual)? {
                            result.push(combined);
                        }
                    }
                }
            }
        }
        BoundJoinKind::LeftOuter => {
            let mut left_matched: HashMap<String, bool> = HashMap::new();

            for r in &right {
                let key_vals: Vec<Value> = right_keys
                    .iter()
                    .map(|k| evaluate_expr(k, r))
                    .collect::<Result<_>>()?;
                let key = values_to_key(&key_vals);
                if let Some(matches) = hash_table.get(&key) {
                    for l in matches {
                        let combined = l.concat(r);
                        if check_residual(&combined, residual)? {
                            result.push(combined);
                            let l_key_vals: Vec<Value> = left_keys
                                .iter()
                                .map(|k| evaluate_expr(k, l))
                                .collect::<Result<_>>()?;
                            left_matched.insert(values_to_key(&l_key_vals), true);
                        }
                    }
                }
            }

            // Add unmatched left rows
            for l in &left {
                let key_vals: Vec<Value> = left_keys
                    .iter()
                    .map(|k| evaluate_expr(k, l))
                    .collect::<Result<_>>()?;
                let key = values_to_key(&key_vals);
                if !left_matched.contains_key(&key) {
                    let null_right =
                        Tuple::new(vec![Value::Null; right.first().map(|r| r.len()).unwrap_or(0)]);
                    result.push(l.concat(&null_right));
                }
            }
        }
        _ => {
            // Fall back to nested loop for other join types
            let condition = BoundJoinCondition::None;
            return execute_nested_loop_join(left, right, &condition, kind);
        }
    }

    Ok(result)
}

fn check_residual(tuple: &Tuple, residual: &Option<BoundExpr>) -> Result<bool> {
    match residual {
        Some(pred) => evaluate_predicate(pred, tuple),
        None => Ok(true),
    }
}

fn execute_hash_aggregate(
    rows: Vec<Tuple>,
    group_by: &[BoundExpr],
    aggregates: &[PhysAggregateExpr],
    schema: &Schema,
) -> Result<(Vec<String>, Vec<Tuple>)> {
    let columns: Vec<String> = schema.columns.iter().map(|c| c.name.clone()).collect();

    // If no grouping and no rows, still produce one row for aggregates
    if group_by.is_empty() && rows.is_empty() {
        let mut values = Vec::new();
        for agg in aggregates {
            let val = compute_aggregate_empty(&agg.kind);
            values.push(val);
        }
        return Ok((columns, vec![Tuple::new(values)]));
    }

    // Group rows by group-by keys (using string key since Value can't be hashed)
    let mut groups: HashMap<String, (Vec<Value>, Vec<Tuple>)> = HashMap::new();
    for row in &rows {
        let key_vals: Vec<Value> = group_by
            .iter()
            .map(|e| evaluate_expr(e, row))
            .collect::<Result<_>>()?;
        let key_str = values_to_key(&key_vals);
        groups
            .entry(key_str)
            .or_insert_with(|| (key_vals, Vec::new()))
            .1
            .push(row.clone());
    }

    // If no grouping, treat all rows as one group
    if group_by.is_empty() {
        groups.insert(String::new(), (vec![], rows));
    }

    let mut result = Vec::new();
    for (_key_str, (key_vals, group_rows)) in groups {
        let mut values = key_vals;
        for agg in aggregates {
            let val = compute_aggregate(&agg.kind, &agg.arg, &group_rows, agg.distinct)?;
            values.push(val);
        }
        result.push(Tuple::new(values));
    }

    Ok((columns, result))
}

fn compute_aggregate_empty(kind: &FunctionKind) -> Value {
    match kind {
        FunctionKind::Count => Value::I64(0),
        FunctionKind::Sum | FunctionKind::Avg | FunctionKind::Min | FunctionKind::Max => {
            Value::Null
        }
        _ => Value::Null,
    }
}

fn compute_aggregate(
    kind: &FunctionKind,
    arg: &Option<BoundExpr>,
    rows: &[Tuple],
    distinct: bool,
) -> Result<Value> {
    let values: Vec<Value> = match arg {
        Some(expr) => rows
            .iter()
            .map(|r| evaluate_expr(expr, r))
            .collect::<Result<_>>()?,
        None => rows.iter().map(|_| Value::I64(1)).collect(),
    };

    let values: Vec<Value> = if distinct {
        let mut seen = Vec::new();
        for v in values {
            if !seen.contains(&v) {
                seen.push(v);
            }
        }
        seen
    } else {
        values
    };

    // Filter out nulls for most aggregates
    let non_null: Vec<&Value> = values.iter().filter(|v| !v.is_null()).collect();

    match kind {
        FunctionKind::Count => Ok(Value::I64(non_null.len() as i64)),

        FunctionKind::Sum => {
            if non_null.is_empty() {
                return Ok(Value::Null);
            }
            let mut sum = 0i64;
            let mut sum_f = 0f64;
            let mut is_float = false;
            for v in non_null {
                match v {
                    Value::I64(n) => sum += n,
                    Value::I32(n) => sum += *n as i64,
                    Value::F64(f) => {
                        is_float = true;
                        sum_f += f;
                    }
                    Value::F32(f) => {
                        is_float = true;
                        sum_f += *f as f64;
                    }
                    _ => {}
                }
            }
            if is_float {
                Ok(Value::F64(sum_f + sum as f64))
            } else {
                Ok(Value::I64(sum))
            }
        }

        FunctionKind::Avg => {
            if non_null.is_empty() {
                return Ok(Value::Null);
            }
            let mut sum = 0f64;
            for v in &non_null {
                if let Some(f) = v.to_f64() {
                    sum += f;
                }
            }
            Ok(Value::F64(sum / non_null.len() as f64))
        }

        FunctionKind::Min => {
            if non_null.is_empty() {
                return Ok(Value::Null);
            }
            let mut min = non_null[0].clone();
            for v in &non_null[1..] {
                if *v < &min {
                    min = (*v).clone();
                }
            }
            Ok(min)
        }

        FunctionKind::Max => {
            if non_null.is_empty() {
                return Ok(Value::Null);
            }
            let mut max = non_null[0].clone();
            for v in &non_null[1..] {
                if *v > &max {
                    max = (*v).clone();
                }
            }
            Ok(max)
        }

        _ => Err(DbError::Unsupported(format!("Aggregate {:?}", kind))),
    }
}

fn execute_insert<'a, A: Accessor>(
    rows: Vec<Tuple>,
    table: &'a catalog::Table,
    table_columns: &'a [catalog::Column],
    target_columns: &'a [catalog::Column],
    accessor: &'a A,
    txn: Txn,
) -> impl Future<Output = Result<u64>> + 'a {
    async move {
        let layout = TupleLayout::from(table_columns.to_vec());
        let mut count = 0u64;

        let mut sorted_cols = table_columns.to_vec();
        sorted_cols.sort_by_key(|c| c.position);

        let col_names: Vec<&str> = sorted_cols.iter().map(|c| c.name.as_ref()).collect();

        for row in rows {
            let mut full_values = Vec::with_capacity(sorted_cols.len());

            for col in &sorted_cols {
                let val = target_columns
                    .iter()
                    .position(|tc| tc.oid == col.oid)
                    .and_then(|idx| row.get(idx).cloned())
                    .unwrap_or(Value::Null);

                if val.is_null() && !col.nullable {
                    return Err(DbError::Internal(format!(
                        "null value in column \"{}\" violates not-null constraint",
                        col.name
                    )));
                }

                if (col.is_unique || col.is_primary_key) && !val.is_null() {
                    let stream = accessor
                        .table_scan(txn, table.oid)
                        .await
                        .map_err(|e| DbError::AccessorError(format!("{:?}", e)))?;
                    let mut stream = std::pin::pin!(stream);
                    while let Some(result) = stream.next().await {
                        let (tuple_bytes, _) = result.map_err(|e| DbError::AccessorError(format!("{:?}", e)))?;
                        if let Some(existing_val) = layout.read_field(&col.name, col.position as usize, &tuple_bytes) {
                            if existing_val == val {
                                return Err(DbError::Internal(format!(
                                    "duplicate key value violates unique constraint on column \"{}\"",
                                    col.name
                                )));
                            }
                        }
                    }
                }

                full_values.push(val);
            }

            let tuple_bytes = layout.encode_tuple(&col_names, &full_values);

            accessor
                .table_insert(txn, table.oid, tuple_bytes)
                .await
                .map_err(|e| DbError::AccessorError(format!("{:?}", e)))?;

            count += 1;
        }

        Ok(count)
    }
}

fn execute_delete<'a, A: Accessor>(
    rows: Vec<Tuple>,
    table: &'a catalog::Table,
    accessor: &'a A,
    txn: Txn,
) -> impl Future<Output = Result<u64>> + 'a {
    async move {
        let mut count = 0u64;

        for row in rows {
            let rid = row.rid.ok_or_else(|| {
                DbError::Internal("Delete requires tuple with RID from table scan".to_string())
            })?;

            accessor
                .table_delete(txn, table.oid, rid)
                .await
                .map_err(|e| DbError::AccessorError(format!("{:?}", e)))?;

            count += 1;
        }

        Ok(count)
    }
}

fn execute_update<'a, A: Accessor>(
    rows: Vec<Tuple>,
    table: &'a catalog::Table,
    table_columns: &'a [catalog::Column],
    assignments: &'a [BoundAssignment],
    accessor: &'a A,
    txn: Txn,
) -> impl Future<Output = Result<u64>> + 'a {
    async move {
        let layout = TupleLayout::from(table_columns.to_vec());
        let mut count = 0u64;

        // 1. Sort columns by position to guarantee perfect alignment
        let mut sorted_cols = table_columns.to_vec();
        sorted_cols.sort_by_key(|c| c.position);

        let col_names: Vec<&str> = sorted_cols.iter().map(|c| c.name.as_ref()).collect();

        for row in rows {
            let rid = row.rid.ok_or_else(|| {
                DbError::Internal("Update requires tuple with RID from table scan".to_string())
            })?;

            // MVCC update: delete old tuple
            accessor
                .table_delete(txn, table.oid, rid)
                .await
                .map_err(|e| DbError::AccessorError(format!("{:?}", e)))?;

            // 2. Rebuild the values array in strictly position-sorted order
            let mut new_values = vec![Value::Null; sorted_cols.len()];

            // Carry over old values safely by position
            for col in &sorted_cols {
                let pos = col.position as usize;
                if pos < row.values.len() {
                    new_values[pos] = row.values[pos].clone();
                }
            }

            // 3. Apply updates
            for assignment in assignments {
                let new_value = evaluate_expr(&assignment.value, &row)?;
                let col_pos = assignment.column.position as usize;
                
                let col = &sorted_cols[col_pos];
                
                if new_value.is_null() && !col.nullable {
                    return Err(DbError::Internal(format!(
                        "null value in column \"{}\" violates not-null constraint",
                        col.name
                    )));
                }

                if (col.is_unique || col.is_primary_key) && !new_value.is_null() && new_value != new_values[col_pos] {
                    let stream = accessor.table_scan(txn, table.oid).await.map_err(|e| DbError::AccessorError(format!("{:?}", e)))?;
                    let mut stream = std::pin::pin!(stream);
                    while let Some(result) = stream.next().await {
                        let (tuple_bytes, _) = result.map_err(|e| DbError::AccessorError(format!("{:?}", e)))?;
                        if let Some(existing_val) = layout.read_field(&col.name, col.position as usize, &tuple_bytes) {
                            if existing_val == new_value {
                                return Err(DbError::Internal(format!(
                                    "duplicate key value violates unique constraint on column \"{}\"",
                                    col.name
                                )));
                            }
                        }
                    }
                }

                if col_pos < new_values.len() {
                    new_values[col_pos] = new_value;
                }
            }
            // 4. Safe encoding
            let tuple_bytes = layout.encode_tuple(&col_names, &new_values);

            accessor
                .table_insert(txn, table.oid, tuple_bytes)
                .await
                .map_err(|e| DbError::AccessorError(format!("{:?}", e)))?;

            count += 1;
        }

        Ok(count)
    }
}
