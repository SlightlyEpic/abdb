//! Uncorrelated subquery materialization.
//!
//! Walks a `BoundStatement` and rewrites `InSubquery`/`Exists`/`Subquery`
//! expressions into literal expressions by pre-executing the inner query.
//! Correlated subqueries are not supported here; the binder already rejects
//! them because outer-scope column resolution is not wired up.

use std::sync::Arc;

use futures::future::BoxFuture;
use futures::FutureExt;

use crate::accessor::Accessor;
use crate::binder::{
    BoundAssignment, BoundExpr, BoundExprKind, BoundInsert, BoundInsertSource, BoundJoin,
    BoundJoinCondition, BoundLiteral, BoundSelect, BoundSelectItem, BoundStatement,
    BoundTableRef, BoundUpdate, BoundDelete, BoundOrderByExpr,
};
use crate::databox::{DataType, Value};
use crate::error::{DbError, Result};
use crate::executor::{execute, ExecutionResult};
use crate::optimizer::Optimizer;
use crate::planner::Planner;
use crate::transaction::Txn;

/// Entry point: rewrite all uncorrelated subqueries in a statement.
pub async fn materialize_subqueries<A: Accessor + 'static>(
    stmt: &mut BoundStatement,
    accessor: Arc<A>,
    txn: Txn,
) -> Result<()> {
    match stmt {
        BoundStatement::Select(sel) => walk_select(sel, &accessor, &txn).await?,
        BoundStatement::Insert(ins) => walk_insert(ins, &accessor, &txn).await?,
        BoundStatement::Update(upd) => walk_update(upd, &accessor, &txn).await?,
        BoundStatement::Delete(del) => walk_delete(del, &accessor, &txn).await?,
        _ => {}
    }
    Ok(())
}

fn walk_select<'a, A: Accessor + 'static>(
    sel: &'a mut BoundSelect,
    accessor: &'a Arc<A>,
    txn: &'a Txn,
) -> BoxFuture<'a, Result<()>> {
    async move {
        for p in sel.projections.iter_mut() {
            walk_expr(&mut p.expr, accessor, txn).await?;
        }
        for t in sel.from.iter_mut() {
            walk_table_ref(t, accessor, txn).await?;
        }
        for j in sel.joins.iter_mut() {
            walk_join(j, accessor, txn).await?;
        }
        if let Some(w) = sel.where_clause.as_mut() {
            walk_expr(w, accessor, txn).await?;
        }
        for g in sel.group_by.iter_mut() {
            walk_expr(g, accessor, txn).await?;
        }
        if let Some(h) = sel.having.as_mut() {
            walk_expr(h, accessor, txn).await?;
        }
        for o in sel.order_by.iter_mut() {
            walk_order(o, accessor, txn).await?;
        }
        if let Some(l) = sel.limit.as_mut() {
            walk_expr(l, accessor, txn).await?;
        }
        if let Some(o) = sel.offset.as_mut() {
            walk_expr(o, accessor, txn).await?;
        }
        Ok(())
    }
    .boxed()
}

fn walk_table_ref<'a, A: Accessor + 'static>(
    t: &'a mut BoundTableRef,
    accessor: &'a Arc<A>,
    txn: &'a Txn,
) -> BoxFuture<'a, Result<()>> {
    async move {
        if let BoundTableRef::Subquery { query, .. } = t {
            walk_select(query, accessor, txn).await?;
        }
        Ok(())
    }
    .boxed()
}

fn walk_join<'a, A: Accessor + 'static>(
    j: &'a mut BoundJoin,
    accessor: &'a Arc<A>,
    txn: &'a Txn,
) -> BoxFuture<'a, Result<()>> {
    async move {
        walk_table_ref(&mut j.table, accessor, txn).await?;
        match &mut j.condition {
            BoundJoinCondition::On(e) => walk_expr(e, accessor, txn).await?,
            BoundJoinCondition::Using(es) | BoundJoinCondition::Natural(es) => {
                for e in es.iter_mut() {
                    walk_expr(e, accessor, txn).await?;
                }
            }
            BoundJoinCondition::None => {}
        }
        Ok(())
    }
    .boxed()
}

fn walk_order<'a, A: Accessor + 'static>(
    o: &'a mut BoundOrderByExpr,
    accessor: &'a Arc<A>,
    txn: &'a Txn,
) -> BoxFuture<'a, Result<()>> {
    async move { walk_expr(&mut o.expr, accessor, txn).await }.boxed()
}

async fn walk_insert<A: Accessor + 'static>(
    ins: &mut BoundInsert,
    accessor: &Arc<A>,
    txn: &Txn,
) -> Result<()> {
    match &mut ins.source {
        BoundInsertSource::Values(rows) => {
            for row in rows.iter_mut() {
                for e in row.iter_mut() {
                    walk_expr(e, accessor, txn).await?;
                }
            }
        }
        BoundInsertSource::Select(sel) => walk_select(sel, accessor, txn).await?,
    }
    Ok(())
}

async fn walk_update<A: Accessor + 'static>(
    upd: &mut BoundUpdate,
    accessor: &Arc<A>,
    txn: &Txn,
) -> Result<()> {
    for a in upd.assignments.iter_mut() {
        walk_expr(&mut a.value, accessor, txn).await?;
    }
    if let Some(w) = upd.where_clause.as_mut() {
        walk_expr(w, accessor, txn).await?;
    }
    Ok(())
}

async fn walk_delete<A: Accessor + 'static>(
    del: &mut BoundDelete,
    accessor: &Arc<A>,
    txn: &Txn,
) -> Result<()> {
    if let Some(w) = del.where_clause.as_mut() {
        walk_expr(w, accessor, txn).await?;
    }
    Ok(())
}

/// Recursively walk an expression, rewriting subquery variants in place.
fn walk_expr<'a, A: Accessor + 'static>(
    expr: &'a mut BoundExpr,
    accessor: &'a Arc<A>,
    txn: &'a Txn,
) -> BoxFuture<'a, Result<()>> {
    async move {
        // Post-order: first walk children (including subquery inner selects).
        match &mut expr.kind {
            BoundExprKind::BinaryOp { left, right, .. } => {
                walk_expr(left, accessor, txn).await?;
                walk_expr(right, accessor, txn).await?;
            }
            BoundExprKind::UnaryOp { expr, .. } => walk_expr(expr, accessor, txn).await?,
            BoundExprKind::IsNull(e) | BoundExprKind::IsNotNull(e) => {
                walk_expr(e, accessor, txn).await?
            }
            BoundExprKind::Like { expr, pattern, .. } => {
                walk_expr(expr, accessor, txn).await?;
                walk_expr(pattern, accessor, txn).await?;
            }
            BoundExprKind::Between { expr, low, high, .. } => {
                walk_expr(expr, accessor, txn).await?;
                walk_expr(low, accessor, txn).await?;
                walk_expr(high, accessor, txn).await?;
            }
            BoundExprKind::In { expr, list, .. } => {
                walk_expr(expr, accessor, txn).await?;
                for e in list.iter_mut() {
                    walk_expr(e, accessor, txn).await?;
                }
            }
            BoundExprKind::Cast { expr, .. } => walk_expr(expr, accessor, txn).await?,
            BoundExprKind::Case { operand, when_then, else_result } => {
                if let Some(op) = operand.as_mut() {
                    walk_expr(op, accessor, txn).await?;
                }
                for (w, t) in when_then.iter_mut() {
                    walk_expr(w, accessor, txn).await?;
                    walk_expr(t, accessor, txn).await?;
                }
                if let Some(er) = else_result.as_mut() {
                    walk_expr(er, accessor, txn).await?;
                }
            }
            BoundExprKind::Function(f) => {
                for a in f.args.iter_mut() {
                    walk_expr(a, accessor, txn).await?;
                }
            }
            BoundExprKind::InSubquery { subquery, .. }
            | BoundExprKind::Exists { subquery, .. } => {
                walk_select(subquery, accessor, txn).await?;
            }
            BoundExprKind::Subquery(subquery) => {
                walk_select(subquery, accessor, txn).await?;
            }
            BoundExprKind::Literal(_) | BoundExprKind::ColumnRef(_) => {}
        }

        // Rewrite this node if it is a subquery variant.
        match std::mem::replace(
            &mut expr.kind,
            BoundExprKind::Literal(BoundLiteral::Null),
        ) {
            BoundExprKind::InSubquery { expr: left, subquery, negated } => {
                let values = execute_subquery_values(*subquery, accessor, txn).await?;
                let list: Vec<BoundExpr> = values
                    .into_iter()
                    .map(|v| value_to_literal_expr(v, left.data_type))
                    .collect();
                expr.kind = BoundExprKind::In {
                    expr: left,
                    list,
                    negated,
                };
                expr.data_type = DataType::Bool;
                expr.nullable = false;
            }
            BoundExprKind::Exists { subquery, negated } => {
                let values = execute_subquery_values(*subquery, accessor, txn).await?;
                let exists = !values.is_empty();
                let b = if negated { !exists } else { exists };
                expr.kind = BoundExprKind::Literal(BoundLiteral::Bool(b));
                expr.data_type = DataType::Bool;
                expr.nullable = false;
            }
            BoundExprKind::Subquery(subquery) => {
                let dt = expr.data_type;
                let values = execute_subquery_values(*subquery, accessor, txn).await?;
                let v = if values.is_empty() {
                    Value::Null
                } else if values.len() > 1 {
                    return Err(DbError::Internal(
                        "scalar subquery returned more than one row".into(),
                    ));
                } else {
                    values.into_iter().next().unwrap()
                };
                let lit = value_to_literal_expr(v, dt);
                *expr = lit;
            }
            other => {
                expr.kind = other; // restore
            }
        }

        Ok(())
    }
    .boxed()
}

/// Execute an uncorrelated subquery and return its first-column values.
async fn execute_subquery_values<A: Accessor + 'static>(
    sel: BoundSelect,
    accessor: &Arc<A>,
    txn: &Txn,
) -> Result<Vec<Value>> {
    let mut stmt = BoundStatement::Select(sel);
    // Recursively materialize any nested subqueries first.
    materialize_subqueries(&mut stmt, Arc::clone(accessor), txn.clone()).await?;

    let sel = match stmt {
        BoundStatement::Select(s) => s,
        _ => unreachable!(),
    };

    let logical = Planner::plan(BoundStatement::Select(sel))
        .map_err(|e| DbError::Internal(format!("plan subquery: {:?}", e)))?;
    let optimizer = Optimizer::new(Arc::clone(accessor), txn.clone());
    let physical = optimizer
        .optimize(logical)
        .map_err(|e| DbError::Internal(format!("optimize subquery: {:?}", e)))?;

    let result = execute(physical, accessor.as_ref(), txn.clone()).await?;

    match result {
        ExecutionResult::Rows { rows, .. } => {
            let mut out = Vec::with_capacity(rows.len());
            for t in rows {
                let first = t.values.into_iter().next().unwrap_or(Value::Null);
                out.push(first);
            }
            Ok(out)
        }
        _ => Err(DbError::Internal("subquery did not return rows".into())),
    }
}

fn value_to_literal_expr(v: Value, target: DataType) -> BoundExpr {
    // Cast the runtime value to the target type so the produced literal
    // matches the data_type tag; otherwise downstream cross-type comparisons
    // silently fail (e.g. AVG's F64 result vs an I64-tagged expression).
    let v = if matches!(v, Value::Null) {
        v
    } else {
        // `Value::cast` can't widen float→int; convert via f64 fallback so
        // an AVG(F64) result lands in an integer-typed literal cleanly.
        let casted = v.cast(target);
        if let Some(c) = casted {
            c
        } else if let Some(f) = v.to_f64() {
            match target {
                DataType::I8 => Value::I8(f as i8),
                DataType::I16 => Value::I16(f as i16),
                DataType::I32 => Value::I32(f as i32),
                DataType::I64 => Value::I64(f as i64),
                DataType::U8 => Value::U8(f as u8),
                DataType::U16 => Value::U16(f as u16),
                DataType::U32 => Value::U32(f as u32),
                DataType::U64 => Value::U64(f as u64),
                DataType::F32 => Value::F32(f as f32),
                DataType::F64 => Value::F64(f),
                _ => v,
            }
        } else {
            v
        }
    };
    let (kind, nullable) = match v {
        Value::Null => (BoundExprKind::Literal(BoundLiteral::Null), true),
        Value::Bool(b) => (BoundExprKind::Literal(BoundLiteral::Bool(b)), false),
        Value::I8(n) => (BoundExprKind::Literal(BoundLiteral::Integer(n as i64)), false),
        Value::I16(n) => (BoundExprKind::Literal(BoundLiteral::Integer(n as i64)), false),
        Value::I32(n) => (BoundExprKind::Literal(BoundLiteral::Integer(n as i64)), false),
        Value::I64(n) => (BoundExprKind::Literal(BoundLiteral::Integer(n)), false),
        Value::U8(n) => (BoundExprKind::Literal(BoundLiteral::Integer(n as i64)), false),
        Value::U16(n) => (BoundExprKind::Literal(BoundLiteral::Integer(n as i64)), false),
        Value::U32(n) => (BoundExprKind::Literal(BoundLiteral::Integer(n as i64)), false),
        Value::U64(n) => (BoundExprKind::Literal(BoundLiteral::Integer(n as i64)), false),
        Value::F32(f) => (BoundExprKind::Literal(BoundLiteral::Float(f as f64)), false),
        Value::F64(f) => (BoundExprKind::Literal(BoundLiteral::Float(f)), false),
        Value::String(s) => (BoundExprKind::Literal(BoundLiteral::String(s)), false),
    };
    BoundExpr {
        kind,
        data_type: target,
        nullable,
    }
}
