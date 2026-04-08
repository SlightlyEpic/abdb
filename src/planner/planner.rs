use crate::{
    binder::{
        BoundColumnRef, BoundDelete, BoundExpr, BoundExprKind, BoundInsert, BoundInsertSource,
        BoundJoin, BoundJoinCondition, BoundSelect, BoundSelectItem, BoundStatement, BoundTableRef,
        BoundUpdate, FunctionKind, OutputColumn,
    },
    databox::DataType,
    error::{DbError, Result},
};

use super::plan::*;

pub struct Planner;

impl Planner {
    pub fn plan(stmt: BoundStatement) -> Result<LogicalPlan> {
        match stmt {
            BoundStatement::Select(s) => Self::plan_select(s),
            BoundStatement::Insert(s) => Self::plan_insert(s),
            BoundStatement::Update(s) => Self::plan_update(s),
            BoundStatement::Delete(s) => Self::plan_delete(s),

            BoundStatement::CreateTable(s) => Ok(LogicalPlan::CreateTable(s)),
            BoundStatement::DropTable(s) => Ok(LogicalPlan::DropTable(s)),
            BoundStatement::AlterTable(s) => Ok(LogicalPlan::AlterTable(s)),

            BoundStatement::CreateIndex(s) => Ok(LogicalPlan::CreateIndex(s)),
            BoundStatement::DropIndex(s) => Ok(LogicalPlan::DropIndex(s)),

            BoundStatement::DescribeTable(t, c) => Ok(LogicalPlan::DescribeTable(t, c)),
            BoundStatement::ShowTables => Ok(LogicalPlan::ShowTables),

            BoundStatement::Explain(inner) => Self::plan(*inner),
        }
    }

    fn plan_select(stmt: BoundSelect) -> Result<LogicalPlan> {
        let mut plan = Self::plan_from(stmt.from, stmt.joins)?;

        if let Some(pred) = stmt.where_clause {
            plan = LogicalPlan::Filter(Filter {
                predicate: pred,
                input: Box::new(plan),
            });
        }

        let has_group_by = !stmt.group_by.is_empty();
        let aggregates = collect_aggregates(&stmt.projections, &stmt.having);
        let has_aggregates = !aggregates.is_empty();

        let agg_schema: Option<Schema>;

        let (projections, having_opt) = if has_group_by || has_aggregates {
            let input_schema = plan.schema();
            let schema = build_aggregate_schema(&stmt.group_by, &aggregates, &input_schema);

            plan = LogicalPlan::Aggregate(Aggregate {
                group_by: stmt.group_by.clone(),
                aggregates: aggregates.clone(),
                input: Box::new(plan),
                schema: schema.clone(),
            });

            agg_schema = Some(schema.clone());

            let rewritten_projections = stmt
                .projections
                .into_iter()
                .map(|p| {
                    let new_expr =
                        rewrite_for_agg_output(p.expr, &stmt.group_by, &aggregates, &schema);
                    BoundSelectItem {
                        expr: new_expr,
                        alias: p.alias,
                    }
                })
                .collect::<Vec<_>>();

            let rewritten_having = stmt
                .having
                .map(|h| rewrite_for_agg_output(h, &stmt.group_by, &aggregates, &schema));

            (rewritten_projections, rewritten_having)
        } else {
            agg_schema = None;
            (stmt.projections, stmt.having)
        };

        if let Some(having) = having_opt {
            plan = LogicalPlan::Filter(Filter {
                predicate: having,
                input: Box::new(plan),
            });
        }

        let proj_exprs: Vec<BoundExpr> = projections.iter().map(|p| p.expr.clone()).collect();

        let (exprs, aliases): (Vec<_>, Vec<_>) = projections
            .into_iter()
            .map(|p| (p.expr, p.alias))
            .unzip();

        let proj_schema = Schema::new(
            stmt.output_columns
                .into_iter()
                .map(|oc| OutputColumn {
                    name: oc.name,
                    data_type: oc.data_type,
                    nullable: oc.nullable,
                })
                .collect(),
        );

        plan = LogicalPlan::Projection(Projection {
            exprs,
            aliases,
            input: Box::new(plan),
            schema: proj_schema,
        });

        if stmt.distinct {
            plan = LogicalPlan::Distinct(Distinct {
                input: Box::new(plan),
            });
        }

        if !stmt.order_by.is_empty() {
            let keys = stmt
                .order_by
                .into_iter()
                .map(|o| {
                    let expr = if let Some(ref schema) = agg_schema {
                        rewrite_for_agg_output(o.expr, &stmt.group_by, &aggregates, schema)
                    } else {
                        o.expr
                    };
                    let expr = rewrite_to_projection_output(expr, &proj_exprs);
                    SortKey {
                        expr,
                        asc: o.asc,
                        nulls_first: o.nulls_first,
                    }
                })
                .collect();
            plan = LogicalPlan::Sort(Sort {
                order_by: keys,
                input: Box::new(plan),
            });
        }

        if stmt.limit.is_some() || stmt.offset.is_some() {
            plan = LogicalPlan::Limit(Limit {
                limit: stmt.limit,
                offset: stmt.offset,
                input: Box::new(plan),
            });
        }

        Ok(plan)
    }

    fn plan_from(from: Vec<BoundTableRef>, joins: Vec<BoundJoin>) -> Result<LogicalPlan> {
        if from.is_empty() {
            return Ok(LogicalPlan::Values(Values {
                rows: vec![vec![]],
                schema: Schema::empty(),
            }));
        }

        let mut plan = Self::plan_table_ref(from.into_iter().next().unwrap())?;

        for join in joins {
            let right = Self::plan_table_ref(join.table)?;
            let left_schema = plan.schema();
            let right_schema = right.schema();
            let schema = merge_schemas(&left_schema, &right_schema);
            plan = LogicalPlan::Join(Join {
                kind: join.kind,
                left: Box::new(plan),
                right: Box::new(right),
                condition: join.condition,
                schema,
            });
        }

        Ok(plan)
    }

    fn plan_table_ref(table_ref: BoundTableRef) -> Result<LogicalPlan> {
        match table_ref {
            BoundTableRef::BaseTable {
                table,
                columns,
                alias,
            } => {
                let schema = Schema::new(
                    columns
                        .iter()
                        .map(|c| OutputColumn {
                            name: alias
                                .as_deref()
                                .map(|a| format!("{}.{}", a, c.name))
                                .unwrap_or_else(|| c.name.to_string()),
                            data_type: c.type_id,
                            nullable: c.nullable,
                        })
                        .collect(),
                );
                Ok(LogicalPlan::SeqScan(SeqScan {
                    table,
                    columns,
                    alias,
                    schema,
                }))
            }
            BoundTableRef::Subquery { query, alias } => {
                let mut plan = Self::plan_select(*query)?;
                if let LogicalPlan::Projection(ref mut p) = plan {
                    for (col, alias_name) in p
                        .schema
                        .columns
                        .iter_mut()
                        .zip(std::iter::repeat(alias.clone()))
                    {
                        col.name = format!("{}.{}", alias_name, col.name);
                    }
                }
                Ok(plan)
            }
        }
    }

    fn plan_insert(stmt: BoundInsert) -> Result<LogicalPlan> {
        let source = match stmt.source {
            BoundInsertSource::Values(rows) => {
                let schema = Schema::new(
                    stmt.target_columns
                        .iter()
                        .map(|c| OutputColumn {
                            name: c.name.to_string(),
                            data_type: c.type_id,
                            nullable: c.nullable,
                        })
                        .collect(),
                );
                LogicalPlan::Values(Values { rows, schema })
            }
            BoundInsertSource::Select(sel) => Self::plan_select(*sel)?,
        };

        let schema = Schema::new(vec![OutputColumn {
            name: "rows_affected".to_string(),
            data_type: DataType::I64,
            nullable: false,
        }]);

        Ok(LogicalPlan::Insert(LogicalInsert {
            table: stmt.table,
            table_columns: stmt.table_columns,
            target_columns: stmt.target_columns,
            source: Box::new(source),
            schema,
        }))
    }

    fn plan_update(stmt: BoundUpdate) -> Result<LogicalPlan> {
        let scan_schema = Schema::new(
            stmt.table_columns
                .iter()
                .map(|c| OutputColumn {
                    name: c.name.to_string(),
                    data_type: c.type_id,
                    nullable: c.nullable,
                })
                .collect(),
        );

        let mut input = LogicalPlan::SeqScan(SeqScan {
            table: stmt.table.clone(),
            columns: stmt.table_columns.clone(),
            alias: None,
            schema: scan_schema,
        });

        if let Some(pred) = stmt.where_clause {
            input = LogicalPlan::Filter(Filter {
                predicate: pred,
                input: Box::new(input),
            });
        }

        let schema = Schema::new(vec![OutputColumn {
            name: "rows_affected".to_string(),
            data_type: DataType::I64,
            nullable: false,
        }]);

        Ok(LogicalPlan::Update(LogicalUpdate {
            table: stmt.table,
            table_columns: stmt.table_columns,
            assignments: stmt.assignments,
            input: Box::new(input),
            schema,
        }))
    }

    fn plan_delete(stmt: BoundDelete) -> Result<LogicalPlan> {
        let scan_schema = Schema::new(
            stmt.table_columns
                .iter()
                .map(|c| OutputColumn {
                    name: c.name.to_string(),
                    data_type: c.type_id,
                    nullable: c.nullable,
                })
                .collect(),
        );

        let mut input = LogicalPlan::SeqScan(SeqScan {
            table: stmt.table.clone(),
            columns: stmt.table_columns.clone(),
            alias: None,
            schema: scan_schema,
        });

        if let Some(pred) = stmt.where_clause {
            input = LogicalPlan::Filter(Filter {
                predicate: pred,
                input: Box::new(input),
            });
        }

        let schema = Schema::new(vec![OutputColumn {
            name: "rows_affected".to_string(),
            data_type: DataType::I64,
            nullable: false,
        }]);

        Ok(LogicalPlan::Delete(LogicalDelete {
            table: stmt.table,
            table_columns: stmt.table_columns,
            input: Box::new(input),
            schema,
        }))
    }
}

fn rewrite_for_agg_output(
    expr: BoundExpr,
    group_by: &[BoundExpr],
    aggregates: &[AggregateExpr],
    agg_schema: &Schema,
) -> BoundExpr {
    match &expr.kind {
        BoundExprKind::Function(f) if f.kind.is_aggregate() => {
            let agg_idx = aggregates.iter().position(|a| {
                a.kind == f.kind
                    && a.distinct == f.distinct
                    && match (&a.arg, f.args.first()) {
                        (None, None) => true,
                        (Some(ae), Some(fe)) => exprs_structurally_equal(ae, fe),
                        _ => false,
                    }
            });
            let slot = agg_idx
                .map(|i| group_by.len() + i)
                .unwrap_or(group_by.len());
            let col_name = agg_schema
                .columns
                .get(slot)
                .map(|c| c.name.clone())
                .unwrap_or_else(|| "?agg?".to_string());
            BoundExpr {
                data_type: expr.data_type,
                nullable: expr.nullable,
                kind: BoundExprKind::ColumnRef(BoundColumnRef {
                    qualifier: None,
                    column_name: col_name,
                    scope_index: slot,
                }),
            }
        }

        BoundExprKind::ColumnRef(cr) => {
            let group_slot = group_by.iter().position(|gb| {
                if let BoundExprKind::ColumnRef(gcr) = &gb.kind {
                    gcr.scope_index == cr.scope_index
                } else {
                    false
                }
            });
            if let Some(slot) = group_slot {
                let col_name = agg_schema
                    .columns
                    .get(slot)
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| cr.column_name.clone());
                BoundExpr {
                    data_type: expr.data_type,
                    nullable: expr.nullable,
                    kind: BoundExprKind::ColumnRef(BoundColumnRef {
                        qualifier: cr.qualifier.clone(),
                        column_name: col_name,
                        scope_index: slot,
                    }),
                }
            } else {
                expr
            }
        }

        BoundExprKind::BinaryOp { left, op, right } => BoundExpr {
            data_type: expr.data_type,
            nullable: expr.nullable,
            kind: BoundExprKind::BinaryOp {
                left: Box::new(rewrite_for_agg_output(
                    *left.clone(),
                    group_by,
                    aggregates,
                    agg_schema,
                )),
                op: op.clone(),
                right: Box::new(rewrite_for_agg_output(
                    *right.clone(),
                    group_by,
                    aggregates,
                    agg_schema,
                )),
            },
        },

        BoundExprKind::UnaryOp { op, expr: inner } => BoundExpr {
            data_type: expr.data_type,
            nullable: expr.nullable,
            kind: BoundExprKind::UnaryOp {
                op: op.clone(),
                expr: Box::new(rewrite_for_agg_output(
                    *inner.clone(),
                    group_by,
                    aggregates,
                    agg_schema,
                )),
            },
        },

        BoundExprKind::Cast {
            expr: inner,
            target_type,
        } => BoundExpr {
            data_type: expr.data_type,
            nullable: expr.nullable,
            kind: BoundExprKind::Cast {
                expr: Box::new(rewrite_for_agg_output(
                    *inner.clone(),
                    group_by,
                    aggregates,
                    agg_schema,
                )),
                target_type: *target_type,
            },
        },

        BoundExprKind::IsNull(inner) => BoundExpr {
            data_type: expr.data_type,
            nullable: expr.nullable,
            kind: BoundExprKind::IsNull(Box::new(rewrite_for_agg_output(
                *inner.clone(),
                group_by,
                aggregates,
                agg_schema,
            ))),
        },

        BoundExprKind::IsNotNull(inner) => BoundExpr {
            data_type: expr.data_type,
            nullable: expr.nullable,
            kind: BoundExprKind::IsNotNull(Box::new(rewrite_for_agg_output(
                *inner.clone(),
                group_by,
                aggregates,
                agg_schema,
            ))),
        },

        _ => expr,
    }
}

fn rewrite_to_projection_output(expr: BoundExpr, proj_exprs: &[BoundExpr]) -> BoundExpr {
    for (proj_pos, proj_expr) in proj_exprs.iter().enumerate() {
        if exprs_structurally_equal(&expr, proj_expr) {
            return BoundExpr {
                data_type: expr.data_type,
                nullable: expr.nullable,
                kind: BoundExprKind::ColumnRef(BoundColumnRef {
                    qualifier: None,
                    column_name: format!("col_{}", proj_pos),
                    scope_index: proj_pos,
                }),
            };
        }
    }

    match expr.kind {
        BoundExprKind::BinaryOp { left, op, right } => BoundExpr {
            data_type: expr.data_type,
            nullable: expr.nullable,
            kind: BoundExprKind::BinaryOp {
                left: Box::new(rewrite_to_projection_output(*left, proj_exprs)),
                op,
                right: Box::new(rewrite_to_projection_output(*right, proj_exprs)),
            },
        },
        BoundExprKind::UnaryOp { op, expr: inner } => BoundExpr {
            data_type: expr.data_type,
            nullable: expr.nullable,
            kind: BoundExprKind::UnaryOp {
                op,
                expr: Box::new(rewrite_to_projection_output(*inner, proj_exprs)),
            },
        },
        other => BoundExpr {
            data_type: expr.data_type,
            nullable: expr.nullable,
            kind: other,
        },
    }
}

fn exprs_structurally_equal(a: &BoundExpr, b: &BoundExpr) -> bool {
    match (&a.kind, &b.kind) {
        (BoundExprKind::ColumnRef(ca), BoundExprKind::ColumnRef(cb)) => {
            ca.scope_index == cb.scope_index
        }
        (BoundExprKind::Literal(la), BoundExprKind::Literal(lb)) => {
            format!("{:?}", la) == format!("{:?}", lb)
        }
        (BoundExprKind::Function(fa), BoundExprKind::Function(fb)) => {
            fa.kind == fb.kind
                && fa.distinct == fb.distinct
                && fa.args.len() == fb.args.len()
                && fa
                    .args
                    .iter()
                    .zip(fb.args.iter())
                    .all(|(x, y)| exprs_structurally_equal(x, y))
        }
        (
            BoundExprKind::BinaryOp {
                left: la,
                op: oa,
                right: ra,
            },
            BoundExprKind::BinaryOp {
                left: lb,
                op: ob,
                right: rb,
            },
        ) => oa == ob && exprs_structurally_equal(la, lb) && exprs_structurally_equal(ra, rb),
        _ => false,
    }
}

fn merge_schemas(left: &Schema, right: &Schema) -> Schema {
    let mut cols = left.columns.clone();
    cols.extend(right.columns.iter().cloned());
    Schema::new(cols)
}

fn collect_aggregates(
    projections: &[BoundSelectItem],
    having: &Option<BoundExpr>,
) -> Vec<AggregateExpr> {
    let mut aggs: Vec<AggregateExpr> = Vec::new();
    for proj in projections {
        collect_agg_from_expr(&proj.expr, &proj.alias, &mut aggs);
    }
    if let Some(h) = having {
        collect_agg_from_expr(h, "having", &mut aggs);
    }
    aggs
}

fn collect_agg_from_expr(expr: &BoundExpr, alias: &str, out: &mut Vec<AggregateExpr>) {
    match &expr.kind {
        BoundExprKind::Function(f) if f.kind.is_aggregate() => {
            let already = out.iter().any(|a| {
                a.alias == alias
                    && a.kind == f.kind
                    && a.distinct == f.distinct
                    && match (&a.arg, f.args.first()) {
                        (None, None) => true,
                        (Some(ae), Some(fe)) => exprs_structurally_equal(ae, fe),
                        _ => false,
                    }
            });
            if !already {
                out.push(AggregateExpr {
                    kind: f.kind.clone(),
                    arg: f.args.first().cloned(),
                    distinct: f.distinct,
                    alias: alias.to_string(),
                    data_type: expr.data_type,
                    nullable: expr.nullable,
                });
            }
        }
        BoundExprKind::BinaryOp { left, right, .. } => {
            collect_agg_from_expr(left, alias, out);
            collect_agg_from_expr(right, alias, out);
        }
        BoundExprKind::UnaryOp { expr, .. } => collect_agg_from_expr(expr, alias, out),
        BoundExprKind::Case {
            operand,
            when_then,
            else_result,
        } => {
            if let Some(op) = operand {
                collect_agg_from_expr(op, alias, out);
            }
            for (w, t) in when_then {
                collect_agg_from_expr(w, alias, out);
                collect_agg_from_expr(t, alias, out);
            }
            if let Some(e) = else_result {
                collect_agg_from_expr(e, alias, out);
            }
        }
        _ => {}
    }
}

fn build_aggregate_schema(
    group_by: &[BoundExpr],
    aggregates: &[AggregateExpr],
    input_schema: &Schema,
) -> Schema {
    let mut cols: Vec<OutputColumn> = Vec::new();

    for (i, expr) in group_by.iter().enumerate() {
        let (name, dt, nullable) = match &expr.kind {
            BoundExprKind::ColumnRef(cr) => {
                let name = input_schema
                    .columns
                    .get(cr.scope_index)
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| cr.column_name.clone());
                (name, expr.data_type, expr.nullable)
            }
            _ => (format!("group_{i}"), expr.data_type, expr.nullable),
        };
        cols.push(OutputColumn {
            name,
            data_type: dt,
            nullable,
        });
    }

    for agg in aggregates {
        cols.push(OutputColumn {
            name: agg.alias.clone(),
            data_type: agg.data_type,
            nullable: agg.nullable,
        });
    }

    Schema::new(cols)
}
