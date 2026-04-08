use std::sync::Arc;

use crate::{
    accessor::Accessor,
    binder::{BoundExpr, BoundExprKind, BoundJoinCondition, BoundJoinKind, FunctionKind, OutputColumn},
    catalog,
    common::txn::Txn,
    databox::DataType,
    error::{DbError, Result},
    parser::ast::{BinaryOp, UnaryOp},
    planner::{
        Aggregate, AggregateExpr, Distinct, Filter, Join, Limit, LogicalDelete, LogicalInsert,
        LogicalPlan, LogicalUpdate, Projection, SeqScan, Sort, SortKey, Values,
    },
};

use super::physical::*;

pub struct Optimizer<A: Accessor> {
    accessor: Arc<A>,
    txn: Txn,
}

impl<A: Accessor> Optimizer<A> {
    pub fn new(accessor: Arc<A>, txn: Txn) -> Self {
        Self { accessor, txn }
    }

    pub fn optimize(&self, plan: LogicalPlan) -> Result<PhysicalPlan> {
        let plan = self.push_predicates(plan, vec![])?;
        let mut phys = self.to_physical(plan)?;
        phys = fuse_sort_limit(phys);
        Ok(phys)
    }

    fn push_predicates(
        &self,
        plan: LogicalPlan,
        mut predicates: Vec<BoundExpr>,
    ) -> Result<LogicalPlan> {
        match plan {
            LogicalPlan::Filter(Filter { predicate, input }) => {
                let mut parts = Vec::new();
                split_conjunctions(predicate, &mut parts);
                predicates.extend(parts);
                self.push_predicates(*input, predicates)
            }

            LogicalPlan::SeqScan(scan) => Ok(LogicalPlan::SeqScan(SeqScan {
                table: scan.table,
                columns: scan.columns,
                alias: scan.alias,
                schema: scan.schema,
            }))
            .map(|scan_plan| {
                predicates.into_iter().rev().fold(scan_plan, |acc, pred| {
                    LogicalPlan::Filter(Filter {
                        predicate: pred,
                        input: Box::new(acc),
                    })
                })
            }),

            LogicalPlan::Projection(p) => {
                let new_input = self.push_predicates(*p.input, predicates)?;
                Ok(LogicalPlan::Projection(Projection {
                    exprs: p.exprs,
                    aliases: p.aliases,
                    schema: p.schema,
                    input: Box::new(new_input),
                }))
            }

            LogicalPlan::Join(join) => {
                let left_col_count = join.left.schema().len();
                let mut left_preds = Vec::new();
                let mut right_preds = Vec::new();
                let mut join_preds = Vec::new();

                for pred in predicates {
                    let refs = collect_scope_indices(&pred);
                    let all_left = refs.iter().all(|&i| i < left_col_count);
                    let all_right = refs.iter().all(|&i| i >= left_col_count);

                    match join.kind {
                        BoundJoinKind::Inner => {
                            if all_left {
                                left_preds.push(pred);
                            } else if all_right {
                                right_preds.push(shift_scope_indices(pred, -(left_col_count as isize)));
                            } else {
                                join_preds.push(pred);
                            }
                        }
                        _ => {
                            join_preds.push(pred);
                        }
                    }
                }

                let left = self.push_predicates(*join.left, left_preds)?;
                let right = self.push_predicates(*join.right, right_preds)?;

                let mut plan = LogicalPlan::Join(Join {
                    kind: join.kind,
                    left: Box::new(left),
                    right: Box::new(right),
                    condition: join.condition,
                    schema: join.schema,
                });
                for pred in join_preds.into_iter().rev() {
                    plan = LogicalPlan::Filter(Filter {
                        predicate: pred,
                        input: Box::new(plan),
                    });
                }
                Ok(plan)
            }

            LogicalPlan::Aggregate(agg) => {
                let new_input = self.push_predicates(*agg.input, vec![])?;
                let mut plan = LogicalPlan::Aggregate(Aggregate {
                    group_by: agg.group_by,
                    aggregates: agg.aggregates,
                    schema: agg.schema,
                    input: Box::new(new_input),
                });
                for pred in predicates.into_iter().rev() {
                    plan = LogicalPlan::Filter(Filter {
                        predicate: pred,
                        input: Box::new(plan),
                    });
                }
                Ok(plan)
            }

            LogicalPlan::Sort(s) => {
                let new_input = self.push_predicates(*s.input, predicates)?;
                Ok(LogicalPlan::Sort(Sort {
                    order_by: s.order_by,
                    input: Box::new(new_input),
                }))
            }

            LogicalPlan::Limit(l) => {
                let new_input = self.push_predicates(*l.input, vec![])?;
                let mut plan = LogicalPlan::Limit(Limit {
                    limit: l.limit,
                    offset: l.offset,
                    input: Box::new(new_input),
                });
                for pred in predicates.into_iter().rev() {
                    plan = LogicalPlan::Filter(Filter {
                        predicate: pred,
                        input: Box::new(plan),
                    });
                }
                Ok(plan)
            }

            LogicalPlan::Distinct(d) => {
                let new_input = self.push_predicates(*d.input, predicates)?;
                Ok(LogicalPlan::Distinct(Distinct {
                    input: Box::new(new_input),
                }))
            }

            LogicalPlan::Update(u) => {
                let new_input = self.push_predicates(*u.input, predicates)?;
                Ok(LogicalPlan::Update(LogicalUpdate {
                    table: u.table,
                    table_columns: u.table_columns,
                    assignments: u.assignments,
                    schema: u.schema,
                    input: Box::new(new_input),
                }))
            }
            LogicalPlan::Delete(d) => {
                let new_input = self.push_predicates(*d.input, predicates)?;
                Ok(LogicalPlan::Delete(LogicalDelete {
                    table: d.table,
                    table_columns: d.table_columns,
                    schema: d.schema,
                    input: Box::new(new_input),
                }))
            }

            other => {
                let mut plan = other;
                for pred in predicates.into_iter().rev() {
                    plan = LogicalPlan::Filter(Filter {
                        predicate: pred,
                        input: Box::new(plan),
                    });
                }
                Ok(plan)
            }
        }
    }

    fn to_physical(&self, plan: LogicalPlan) -> Result<PhysicalPlan> {
        match plan {
            LogicalPlan::Nothing => Ok(PhysicalPlan::Nothing),

            LogicalPlan::CreateTable(s) => Ok(PhysicalPlan::CreateTable(s)),
            LogicalPlan::DropTable(s)   => Ok(PhysicalPlan::DropTable(s)),
            LogicalPlan::AlterTable(s)  => Ok(PhysicalPlan::AlterTable(s)),

            LogicalPlan::CreateIndex(s) => Ok(PhysicalPlan::CreateIndex(s)),
            LogicalPlan::DropIndex(s)   => Ok(PhysicalPlan::DropIndex(s)),

            LogicalPlan::DescribeTable(t, c) => Ok(PhysicalPlan::DescribeTable(t, c)),
            LogicalPlan::ShowTables          => Ok(PhysicalPlan::ShowTables),

            LogicalPlan::Values(v) => Ok(PhysicalPlan::Values(PhysValues {
                rows: v.rows,
                schema: v.schema,
            })),

            LogicalPlan::Filter(Filter { predicate, input })
                if matches!(*input, LogicalPlan::SeqScan(_)) =>
            {
                let LogicalPlan::SeqScan(scan) = *input else {
                    unreachable!()
                };
                let mut parts = Vec::new();
                split_conjunctions(predicate, &mut parts);

                if let Some(phys_index) = self.try_index_scan(&scan, &parts)? {
                    return Ok(PhysicalPlan::IndexScan(phys_index));
                }

                let combined = conjoin(parts);
                Ok(PhysicalPlan::Filter(PhysFilter {
                    predicate: combined,
                    input: Box::new(PhysicalPlan::SeqScan(PhysSeqScan {
                        table: scan.table,
                        columns: scan.columns,
                        alias: scan.alias,
                        pushed_predicates: vec![],
                        schema: scan.schema,
                    })),
                }))
            }

            LogicalPlan::SeqScan(scan) => Ok(PhysicalPlan::SeqScan(PhysSeqScan {
                table: scan.table,
                columns: scan.columns,
                alias: scan.alias,
                pushed_predicates: vec![],
                schema: scan.schema,
            })),

            LogicalPlan::IndexScan(scan) => Ok(PhysicalPlan::IndexScan(PhysIndexScan {
                index: scan.index,
                table: scan.table,
                columns: scan.columns,
                start_key: scan.start_key,
                end_key: scan.end_key,
                residual_predicates: vec![],
                schema: scan.schema,
            })),

            LogicalPlan::Filter(f) => {
                let input = self.to_physical(*f.input)?;
                Ok(PhysicalPlan::Filter(PhysFilter {
                    predicate: f.predicate,
                    input: Box::new(input),
                }))
            }

            LogicalPlan::Projection(p) => {
                let input = self.to_physical(*p.input)?;
                Ok(PhysicalPlan::Projection(PhysProjection {
                    exprs: p.exprs,
                    aliases: p.aliases,
                    schema: p.schema,
                    input: Box::new(input),
                }))
            }

            LogicalPlan::Join(join) => {
                let left = self.to_physical(*join.left)?;
                let right = self.to_physical(*join.right)?;
                let schema = join.schema;
                let kind = join.kind;

                if let Some((lk, rk, residual)) = extract_equi_keys(&join.condition) {
                    let left_col_count = left.schema().len();
                    let right_keys = rk
                        .into_iter()
                        .map(|e| shift_scope_indices(e, -(left_col_count as isize)))
                        .collect();
                    return Ok(PhysicalPlan::HashJoin(PhysHashJoin {
                        kind,
                        left: Box::new(left),
                        right: Box::new(right),
                        left_keys: lk,
                        right_keys,
                        residual,
                        schema,
                    }));
                }

                Ok(PhysicalPlan::NestedLoopJoin(PhysNestedLoopJoin {
                    kind,
                    left: Box::new(left),
                    right: Box::new(right),
                    condition: join.condition,
                    schema,
                }))
            }

            LogicalPlan::Aggregate(agg) => {
                let input = self.to_physical(*agg.input)?;
                let agg_exprs = agg
                    .aggregates
                    .into_iter()
                    .map(|a| PhysAggregateExpr {
                        kind: a.kind,
                        arg: a.arg,
                        distinct: a.distinct,
                        alias: a.alias,
                        data_type: a.data_type,
                        nullable: a.nullable,
                    })
                    .collect();
                Ok(PhysicalPlan::HashAggregate(PhysHashAggregate {
                    group_by: agg.group_by,
                    aggregates: agg_exprs,
                    schema: agg.schema,
                    input: Box::new(input),
                }))
            }

            LogicalPlan::Sort(s) => {
                let input = self.to_physical(*s.input)?;
                let keys = s
                    .order_by
                    .into_iter()
                    .map(|k| PhysSortKey {
                        expr: k.expr,
                        asc: k.asc,
                        nulls_first: k.nulls_first,
                    })
                    .collect();
                Ok(PhysicalPlan::Sort(PhysSort {
                    order_by: keys,
                    input: Box::new(input),
                }))
            }

            LogicalPlan::Limit(l) => {
                let input = self.to_physical(*l.input)?;
                Ok(PhysicalPlan::Limit(PhysLimit {
                    limit: l.limit,
                    offset: l.offset,
                    input: Box::new(input),
                }))
            }

            LogicalPlan::Distinct(d) => {
                let input = self.to_physical(*d.input)?;
                Ok(PhysicalPlan::HashDistinct(PhysHashDistinct {
                    input: Box::new(input),
                }))
            }

            LogicalPlan::Insert(ins) => {
                let source = self.to_physical(*ins.source)?;
                Ok(PhysicalPlan::Insert(PhysInsert {
                    table: ins.table,
                    table_columns: ins.table_columns,
                    target_columns: ins.target_columns,
                    schema: ins.schema,
                    source: Box::new(source),
                }))
            }

            LogicalPlan::Update(u) => {
                let input = self.to_physical(*u.input)?;
                Ok(PhysicalPlan::Update(PhysUpdate {
                    table: u.table,
                    table_columns: u.table_columns,
                    assignments: u.assignments,
                    schema: u.schema,
                    input: Box::new(input),
                }))
            }

            LogicalPlan::Delete(d) => {
                let input = self.to_physical(*d.input)?;
                Ok(PhysicalPlan::Delete(PhysDelete {
                    table: d.table,
                    table_columns: d.table_columns,
                    schema: d.schema,
                    input: Box::new(input),
                }))
            }
        }
    }

    //

    fn try_index_scan(
        &self,
        scan: &crate::planner::SeqScan,
        predicates: &[BoundExpr],
    ) -> Result<Option<PhysIndexScan>> {
        for (pred_idx, pred) in predicates.iter().enumerate() {
            if let Some((col_scope_idx, op, key_expr)) = extract_index_predicate(pred) {
                let Some(col) = scan.columns.get(col_scope_idx) else {
                    continue;
                };

                let index = match self.find_index_for_column(scan.table.oid, col.oid) {
                    Some(idx) => idx,
                    None => continue,
                };

                let (start_key, end_key) = range_from_op(op, key_expr);

                let residual: Vec<BoundExpr> = predicates
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != pred_idx)
                    .map(|(_, p)| p.clone())
                    .collect();

                return Ok(Some(PhysIndexScan {
                    index,
                    table: scan.table.clone(),
                    columns: scan.columns.clone(),
                    start_key,
                    end_key,
                    residual_predicates: residual,
                    schema: scan.schema.clone(),
                }));
            }
        }
        Ok(None)
    }

    fn find_index_for_column(
        &self,
        table_oid: crate::common::aliases::OId,
        column_oid: crate::common::aliases::OId,
    ) -> Option<catalog::Index> {
        if let Ok(indexes) = self.accessor.catalog_get_table_indexes(self.txn, table_oid) {
            return indexes.into_iter().find(|idx| idx.column_oid == column_oid);
        }
        None
    }
}

fn fuse_sort_limit(plan: PhysicalPlan) -> PhysicalPlan {
    match plan {
        PhysicalPlan::Limit(PhysLimit {
            limit: Some(limit_expr),
            offset,
            input,
        }) => match *input {
            PhysicalPlan::Sort(PhysSort {
                order_by,
                input: sort_input,
            }) => {
                let sort_input = fuse_sort_limit(*sort_input);
                PhysicalPlan::TopN(PhysTopN {
                    order_by,
                    limit: limit_expr,
                    offset,
                    input: Box::new(sort_input),
                })
            }
            other => {
                let other = fuse_sort_limit(other);
                PhysicalPlan::Limit(PhysLimit {
                    limit: Some(limit_expr),
                    offset,
                    input: Box::new(other),
                })
            }
        },
        PhysicalPlan::Filter(n) => PhysicalPlan::Filter(PhysFilter {
            predicate: n.predicate,
            input: Box::new(fuse_sort_limit(*n.input)),
        }),
        PhysicalPlan::Projection(n) => PhysicalPlan::Projection(PhysProjection {
            exprs: n.exprs,
            aliases: n.aliases,
            schema: n.schema,
            input: Box::new(fuse_sort_limit(*n.input)),
        }),
        PhysicalPlan::NestedLoopJoin(n) => PhysicalPlan::NestedLoopJoin(PhysNestedLoopJoin {
            kind: n.kind,
            left: Box::new(fuse_sort_limit(*n.left)),
            right: Box::new(fuse_sort_limit(*n.right)),
            condition: n.condition,
            schema: n.schema,
        }),
        PhysicalPlan::HashJoin(n) => PhysicalPlan::HashJoin(PhysHashJoin {
            kind: n.kind,
            left: Box::new(fuse_sort_limit(*n.left)),
            right: Box::new(fuse_sort_limit(*n.right)),
            left_keys: n.left_keys,
            right_keys: n.right_keys,
            residual: n.residual,
            schema: n.schema,
        }),
        PhysicalPlan::HashAggregate(n) => PhysicalPlan::HashAggregate(PhysHashAggregate {
            group_by: n.group_by,
            aggregates: n.aggregates,
            schema: n.schema,
            input: Box::new(fuse_sort_limit(*n.input)),
        }),
        PhysicalPlan::Sort(n) => PhysicalPlan::Sort(PhysSort {
            order_by: n.order_by,
            input: Box::new(fuse_sort_limit(*n.input)),
        }),
        PhysicalPlan::Limit(PhysLimit {
            limit,
            offset,
            input,
        }) => PhysicalPlan::Limit(PhysLimit {
            limit,
            offset,
            input: Box::new(fuse_sort_limit(*input)),
        }),
        PhysicalPlan::Distinct(n) => PhysicalPlan::Distinct(PhysDistinct {
            input: Box::new(fuse_sort_limit(*n.input)),
        }),
        PhysicalPlan::HashDistinct(n) => PhysicalPlan::HashDistinct(PhysHashDistinct {
            input: Box::new(fuse_sort_limit(*n.input)),
        }),
        PhysicalPlan::Insert(n) => PhysicalPlan::Insert(PhysInsert {
            source: Box::new(fuse_sort_limit(*n.source)),
            ..n
        }),
        PhysicalPlan::Update(n) => PhysicalPlan::Update(PhysUpdate {
            input: Box::new(fuse_sort_limit(*n.input)),
            ..n
        }),
        PhysicalPlan::Delete(n) => PhysicalPlan::Delete(PhysDelete {
            input: Box::new(fuse_sort_limit(*n.input)),
            ..n
        }),
        leaf => leaf,
    }
}

fn split_conjunctions(expr: BoundExpr, out: &mut Vec<BoundExpr>) {
    match expr.kind {
        BoundExprKind::BinaryOp {
            left,
            op: BinaryOp::And,
            right,
        } => {
            split_conjunctions(*left, out);
            split_conjunctions(*right, out);
        }
        _ => out.push(expr),
    }
}

fn conjoin(mut exprs: Vec<BoundExpr>) -> BoundExpr {
    assert!(!exprs.is_empty());
    let mut result = exprs.remove(0);
    for e in exprs {
        result = BoundExpr {
            nullable: result.nullable || e.nullable,
            kind: BoundExprKind::BinaryOp {
                left: Box::new(result),
                op: BinaryOp::And,
                right: Box::new(e),
            },
            data_type: DataType::Bool,
        };
    }
    result
}

fn collect_scope_indices(expr: &BoundExpr) -> Vec<usize> {
    let mut out = Vec::new();
    collect_scope_indices_inner(expr, &mut out);
    out
}

fn collect_scope_indices_inner(expr: &BoundExpr, out: &mut Vec<usize>) {
    match &expr.kind {
        BoundExprKind::ColumnRef(cr) => out.push(cr.scope_index),
        BoundExprKind::BinaryOp { left, right, .. } => {
            collect_scope_indices_inner(left, out);
            collect_scope_indices_inner(right, out);
        }
        BoundExprKind::UnaryOp { expr, .. } => collect_scope_indices_inner(expr, out),
        BoundExprKind::IsNull(e) | BoundExprKind::IsNotNull(e) => {
            collect_scope_indices_inner(e, out);
        }
        BoundExprKind::Between {
            expr, low, high, ..
        } => {
            collect_scope_indices_inner(expr, out);
            collect_scope_indices_inner(low, out);
            collect_scope_indices_inner(high, out);
        }
        BoundExprKind::In { expr, list, .. } => {
            collect_scope_indices_inner(expr, out);
            for e in list {
                collect_scope_indices_inner(e, out);
            }
        }
        BoundExprKind::Function(f) => {
            for a in &f.args {
                collect_scope_indices_inner(a, out);
            }
        }
        BoundExprKind::Cast { expr, .. } => collect_scope_indices_inner(expr, out),
        BoundExprKind::Case {
            operand,
            when_then,
            else_result,
        } => {
            if let Some(op) = operand {
                collect_scope_indices_inner(op, out);
            }
            for (w, t) in when_then {
                collect_scope_indices_inner(w, out);
                collect_scope_indices_inner(t, out);
            }
            if let Some(e) = else_result {
                collect_scope_indices_inner(e, out);
            }
        }
        _ => {}
    }
}

fn shift_scope_indices(expr: BoundExpr, delta: isize) -> BoundExpr {
    BoundExpr {
        data_type: expr.data_type,
        nullable: expr.nullable,
        kind: shift_kind(expr.kind, delta),
    }
}

fn shift_kind(kind: BoundExprKind, delta: isize) -> BoundExprKind {
    match kind {
        BoundExprKind::ColumnRef(mut cr) => {
            cr.scope_index = (cr.scope_index as isize + delta) as usize;
            BoundExprKind::ColumnRef(cr)
        }
        BoundExprKind::BinaryOp { left, op, right } => BoundExprKind::BinaryOp {
            left: Box::new(shift_scope_indices(*left, delta)),
            op,
            right: Box::new(shift_scope_indices(*right, delta)),
        },
        BoundExprKind::UnaryOp { op, expr } => BoundExprKind::UnaryOp {
            op,
            expr: Box::new(shift_scope_indices(*expr, delta)),
        },
        BoundExprKind::IsNull(e) => BoundExprKind::IsNull(Box::new(shift_scope_indices(*e, delta))),
        BoundExprKind::IsNotNull(e) => {
            BoundExprKind::IsNotNull(Box::new(shift_scope_indices(*e, delta)))
        }
        other => other,
    }
}

fn extract_index_predicate(pred: &BoundExpr) -> Option<(usize, BinaryOp, BoundExpr)> {
    let BoundExprKind::BinaryOp { left, op, right } = &pred.kind else {
        return None;
    };
    match op {
        BinaryOp::Eq | BinaryOp::Lt | BinaryOp::LtEq | BinaryOp::Gt | BinaryOp::GtEq => {}
        _ => return None,
    }
    if let BoundExprKind::ColumnRef(cr) = &left.kind {
        if is_literal(right) {
            return Some((cr.scope_index, op.clone(), *right.clone()));
        }
    }
    if let BoundExprKind::ColumnRef(cr) = &right.kind {
        if is_literal(left) {
            let flipped = flip_op(op)?;
            return Some((cr.scope_index, flipped, *left.clone()));
        }
    }
    None
}

fn is_literal(expr: &BoundExpr) -> bool {
    matches!(expr.kind, BoundExprKind::Literal(_))
}

fn flip_op(op: &BinaryOp) -> Option<BinaryOp> {
    match op {
        BinaryOp::Lt => Some(BinaryOp::Gt),
        BinaryOp::LtEq => Some(BinaryOp::GtEq),
        BinaryOp::Gt => Some(BinaryOp::Lt),
        BinaryOp::GtEq => Some(BinaryOp::LtEq),
        BinaryOp::Eq => Some(BinaryOp::Eq),
        _ => None,
    }
}

fn range_from_op(op: BinaryOp, key: BoundExpr) -> (Option<BoundExpr>, Option<BoundExpr>) {
    match op {
        BinaryOp::Eq => (Some(key.clone()), Some(key)),
        BinaryOp::Gt | BinaryOp::GtEq => (Some(key), None),
        BinaryOp::Lt | BinaryOp::LtEq => (None, Some(key)),
        _ => (None, None),
    }
}

///
fn extract_equi_keys(
    condition: &BoundJoinCondition,
) -> Option<(Vec<BoundExpr>, Vec<BoundExpr>, Option<BoundExpr>)> {
    match condition {
        BoundJoinCondition::On(expr) => {
            let mut parts = Vec::new();
            split_conjunctions(expr.clone(), &mut parts);

            let mut left_keys = Vec::new();
            let mut right_keys = Vec::new();
            let mut residual = Vec::new();

            for part in parts {
                if let BoundExprKind::BinaryOp {
                    left,
                    op: BinaryOp::Eq,
                    right,
                } = &part.kind
                {
                    let left_refs = collect_scope_indices(left);
                    let right_refs = collect_scope_indices(right);
                    if !left_refs.is_empty() && !right_refs.is_empty() {
                        if left_refs
                            .iter()
                            .all(|&i| right_refs.iter().all(|&j| i != j))
                        {
                            left_keys.push(*left.clone());
                            right_keys.push(*right.clone());
                            continue;
                        }
                    }
                }
                residual.push(part);
            }

            if left_keys.is_empty() {
                return None;
            }

            let residual_expr = if residual.is_empty() {
                None
            } else {
                Some(conjoin(residual))
            };

            Some((left_keys, right_keys, residual_expr))
        }
        BoundJoinCondition::Using(exprs) | BoundJoinCondition::Natural(exprs) => {
            let mut left_keys = Vec::new();
            let mut right_keys = Vec::new();
            for expr in exprs {
                if let BoundExprKind::BinaryOp {
                    left,
                    op: BinaryOp::Eq,
                    right,
                } = &expr.kind
                {
                    left_keys.push(*left.clone());
                    right_keys.push(*right.clone());
                }
            }
            if left_keys.is_empty() {
                None
            } else {
                Some((left_keys, right_keys, None))
            }
        }
        BoundJoinCondition::None => None,
    }
}
