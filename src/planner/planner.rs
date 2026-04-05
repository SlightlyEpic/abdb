use crate::{
    binder::{
        BoundDelete, BoundExpr, BoundExprKind, BoundInsert, BoundInsertSource, BoundJoin,
        BoundJoinCondition, BoundSelect, BoundSelectItem, BoundStatement, BoundTableRef,
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
            BoundStatement::BeginTransaction => Ok(LogicalPlan::Nothing),
            BoundStatement::Commit => Ok(LogicalPlan::Nothing),
            BoundStatement::Rollback => Ok(LogicalPlan::Nothing),

            BoundStatement::Select(s) => Self::plan_select(s),
            BoundStatement::Insert(s) => Self::plan_insert(s),
            BoundStatement::Update(s) => Self::plan_update(s),
            BoundStatement::Delete(s) => Self::plan_delete(s),

            BoundStatement::CreateTable(s) => Ok(LogicalPlan::CreateTable(s)),
            BoundStatement::DropTable(s) => Ok(LogicalPlan::DropTable(s)),
            BoundStatement::AlterTable(s) => Ok(LogicalPlan::AlterTable(s)),
            BoundStatement::CreateIndex(s) => Ok(LogicalPlan::CreateIndex(s)),
            BoundStatement::DropIndex(s) => Ok(LogicalPlan::DropIndex(s)),

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

        if has_group_by || has_aggregates {
            let agg_schema = build_aggregate_schema(&stmt.group_by, &aggregates, &plan.schema());
            plan = LogicalPlan::Aggregate(Aggregate {
                group_by: stmt.group_by,
                aggregates,
                input: Box::new(plan),
                schema: agg_schema,
            });
        }

        if let Some(having) = stmt.having {
            plan = LogicalPlan::Filter(Filter {
                predicate: having,
                input: Box::new(plan),
            });
        }

        let (exprs, aliases): (Vec<_>, Vec<_>) = stmt
            .projections
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
                .map(|o| SortKey {
                    expr: o.expr,
                    asc: o.asc,
                    nulls_first: o.nulls_first,
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
            let already = out.iter().any(|a| a.alias == alias);
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
