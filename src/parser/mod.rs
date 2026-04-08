pub mod ast;

use sqlparser::ast as sq;
use sqlparser::ast::ForeignKeyConstraint;
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser as SqParser;

use crate::databox::DataType;
use crate::error::{DbError, Result};
use crate::transaction::IsolationLevel;
use ast::*;

pub struct Parser;

impl Parser {
    pub fn parse(sql: &str) -> Result<Vec<Statement>> {
        let dialect = GenericDialect {};
        let stmts =
            SqParser::parse_sql(&dialect, sql).map_err(|e| DbError::Parse(e.to_string()))?;
        stmts.into_iter().map(translate_statement).collect()
    }
}

fn translate_statement(stmt: sq::Statement) -> Result<Statement> {
    match stmt {
        sq::Statement::StartTransaction {
            modes,
            modifier,
            statements,
            exception,
            .. // Rest of these are syntactical variations.
        } => {
            let isolation_level = match modes.as_slice() {
                [] => None,

                [sq::TransactionMode::IsolationLevel(isl)] => Some(match isl {
                    sq::TransactionIsolationLevel::ReadCommitted    => IsolationLevel::ReadCommitted,
                    sq::TransactionIsolationLevel::ReadUncommitted  => IsolationLevel::ReadUncommitted,
                    sq::TransactionIsolationLevel::RepeatableRead   => IsolationLevel::RepeatableRead,
                    sq::TransactionIsolationLevel::Snapshot         => IsolationLevel::Snapshot,
                    sq::TransactionIsolationLevel::Serializable     => IsolationLevel::Serializable,
                }),

                [mode] => {
                    return Err(DbError::Parse(format!(
                        "unsupported transaction mode: {}",
                        mode
                    )));
                }

                _ => {
                    return Err(DbError::Parse(
                        "too many transaction modes specified".into(),
                    ));
                }
            };

            if modifier.is_some() {
                return Err(DbError::Parse("transaction modifiers are not supported".into()));
            }

            if !statements.is_empty() {
                return Err(DbError::Parse("transaction statements are not supported".into()));
            }

            if exception.is_some() {
                return Err(DbError::Parse("transaction exceptions are not supported".into()));
            }

            Ok(Statement::BeginTransaction(isolation_level))
        }
        sq::Statement::Commit { .. } => Ok(Statement::Commit),
        sq::Statement::Rollback { .. } => Ok(Statement::Rollback),

        sq::Statement::CreateTable(ct) => translate_create_table(ct),

        sq::Statement::Drop {
            object_type: sq::ObjectType::Table,
            names,
            if_exists,
            ..
        } => {
            if names.len() != 1 {
                return Err(DbError::Parse(format!(
                    "expected exactly one table name in DROP TABLE, got: {:?}",
                    names
                )));
            }
            let name = names.into_iter().next().unwrap().to_string();
            Ok(Statement::DropTable(DropTableStmt { name, if_exists }))
        }

        sq::Statement::AlterTable(at) => {
            if at.operations.is_empty() {
                return Err(DbError::Parse("ALTER TABLE has no operations".to_string()));
            }
            if at.operations.len() > 1 {
                return Err(DbError::Parse(format!(
                    "ALTER TABLE with multiple operations is not supported (got {})",
                    at.operations.len()
                )));
            }
            let action = translate_alter_action(&at.operations[0])?;
            let name = at.name.to_string();
            if name.trim().is_empty() {
                return Err(DbError::Parse("ALTER TABLE requires a table name".into()));
            }
            Ok(Statement::AlterTable(AlterTableStmt { name, action }))
        }

        sq::Statement::Insert(insert) => translate_insert(insert),

        sq::Statement::Update(sq::Update {
            table,
            assignments,
            selection,
            ..
        }) => translate_update(table, assignments, selection),

        sq::Statement::Delete(delete) => translate_delete(delete),

        sq::Statement::Query(q) => {
            let sel = translate_query(&q)?;
            Ok(Statement::Select(sel))
        }

        sq::Statement::CreateIndex(ci) => {
            let table = ci.table_name.to_string();
            let name = ci
                .name
                .map(|n| n.to_string())
                .ok_or_else(|| DbError::Parse("CREATE INDEX requires an index name".into()))?;
            let columns = ci.columns.iter().map(|c| c.column.to_string()).collect();
            Ok(Statement::CreateIndex(CreateIndexStmt {
                name,
                table,
                columns,
                unique: ci.unique,
                if_not_exists: ci.if_not_exists,
            }))
        }

        sq::Statement::Drop {
            object_type: sq::ObjectType::Index,
            names,
            table,
            if_exists,
            ..
        } => {
            let table_name = table
                .ok_or_else(|| DbError::Parse("DROP INDEX missing table name".into()))?
                .to_string();
            let name = names
                .into_iter()
                .next()
                .ok_or_else(|| DbError::Parse("DROP INDEX missing index name".into()))?
                .to_string();

            if name.trim().is_empty() {
                return Err(DbError::Parse("DROP INDEX requires index name".into()));
            }
            Ok(Statement::DropIndex(DropIndexStmt {
                name,
                table: table_name,
                if_exists,
            }))
        }

        sq::Statement::Explain { statement, .. } => {
            let inner = translate_statement(*statement)?;
            Ok(Statement::Explain(Box::new(inner)))
        }

        other => Err(DbError::Parse(format!(
            "unsupported statement: {:?}",
            other
        ))),
    }
}

fn translate_create_table(ct: sq::CreateTable) -> Result<Statement> {
    let name = ct.name.to_string();
    if name.trim().is_empty() {
        return Err(DbError::Parse("CREATE TABLE requires a table name".into()));
    }
    let if_not_exists = ct.if_not_exists;
    let mut columns: Vec<ColumnDef> = Vec::new();
    let mut primary_key: Vec<String> = Vec::new();
    let mut foreign_keys: Vec<ForeignKeyClause> = Vec::new();

    for col in &ct.columns {
        let data_type = translate_data_type(&col.data_type)?;
        let mut nullable = true;
        let mut is_pk = false;
        let mut is_unique = false;
        let mut default = None;

        for opt in &col.options {
            match &opt.option {
                sq::ColumnOption::NotNull => nullable = false,
                sq::ColumnOption::Null => nullable = true,
                sq::ColumnOption::PrimaryKey(_) => {
                    is_pk = true;
                    nullable = false;
                }
                sq::ColumnOption::Unique(_) => {
                    is_unique = true;
                }
                sq::ColumnOption::Default(expr) => {
                    default = Some(translate_expr(expr)?);
                }
                sq::ColumnOption::ForeignKey(fk) => {
                    foreign_keys.push(translate_fk_constraint_col(col.name.value.clone(), fk));
                }
                _ => {}
            }
        }

        if is_pk {
            primary_key.push(col.name.value.clone());
        }

        columns.push(ColumnDef {
            name: col.name.value.clone(),
            data_type,
            nullable,
            default,
            primary_key: is_pk,
            unique: is_unique,
        });
    }

    for constraint in &ct.constraints {
        match constraint {
            sq::TableConstraint::PrimaryKey(pk) => {
                primary_key = pk.columns.iter().map(|c| c.column.to_string()).collect();
                for col_name in &primary_key {
                    if let Some(c) = columns
                        .iter_mut()
                        .find(|c| c.name.eq_ignore_ascii_case(col_name))
                    {
                        c.primary_key = true;
                        c.nullable = false;
                    }
                }
            }
            sq::TableConstraint::ForeignKey(fk) => {
                foreign_keys.push(translate_fk_constraint(fk));
            }
            sq::TableConstraint::Unique(u) => {
                for col in &u.columns {
                    if let Some(c) = columns
                        .iter_mut()
                        .find(|c| c.name.eq_ignore_ascii_case(&col.column.to_string()))
                    {
                        c.unique = true;
                    }
                }
            }
            _ => {}
        }
    }

    Ok(Statement::CreateTable(CreateTableStmt {
        name,
        columns,
        primary_key,
        foreign_keys,
        if_not_exists,
    }))
}

fn translate_fk_constraint_col(col_name: String, fk: &ForeignKeyConstraint) -> ForeignKeyClause {
    ForeignKeyClause {
        columns: vec![col_name],
        ref_table: fk.foreign_table.to_string(),
        ref_columns: fk
            .referred_columns
            .iter()
            .map(|c| c.value.clone())
            .collect(),
        on_delete: fk.on_delete.as_ref().map(translate_fk_action),
        on_update: fk.on_update.as_ref().map(translate_fk_action),
    }
}

fn translate_fk_constraint(fk: &ForeignKeyConstraint) -> ForeignKeyClause {
    ForeignKeyClause {
        columns: fk.columns.iter().map(|c| c.value.clone()).collect(),
        ref_table: fk.foreign_table.to_string(),
        ref_columns: fk
            .referred_columns
            .iter()
            .map(|c| c.value.clone())
            .collect(),
        on_delete: fk.on_delete.as_ref().map(translate_fk_action),
        on_update: fk.on_update.as_ref().map(translate_fk_action),
    }
}

fn translate_fk_action(action: &sq::ReferentialAction) -> ForeignKeyAction {
    match action {
        sq::ReferentialAction::Cascade => ForeignKeyAction::Cascade,
        sq::ReferentialAction::SetNull => ForeignKeyAction::SetNull,
        sq::ReferentialAction::Restrict => ForeignKeyAction::Restrict,
        sq::ReferentialAction::NoAction => ForeignKeyAction::NoAction,
        _ => ForeignKeyAction::NoAction,
    }
}

fn translate_alter_action(op: &sq::AlterTableOperation) -> Result<AlterTableAction> {
    match op {
        sq::AlterTableOperation::AddColumn { column_def, .. } => {
            let dt = translate_data_type(&column_def.data_type)?;
            let mut nullable = true;
            let mut is_pk = false;
            let mut is_unique = false;
            let mut default = None;

            for opt in &column_def.options {
                match &opt.option {
                    sq::ColumnOption::NotNull => nullable = false,
                    sq::ColumnOption::Null => nullable = true,
                    sq::ColumnOption::PrimaryKey(_) => {
                        is_pk = true;
                        nullable = false;
                    }
                    sq::ColumnOption::Unique(_) => is_unique = true,
                    sq::ColumnOption::Default(expr) => {
                        default = Some(translate_expr(expr)?);
                    }
                    _ => {}
                }
            }

            Ok(AlterTableAction::AddColumn(ColumnDef {
                name: column_def.name.value.clone(),
                data_type: dt,
                nullable,
                default,
                primary_key: is_pk,
                unique: is_unique,
            }))
        }

        sq::AlterTableOperation::DropColumn { column_names, .. } => {
            let name = column_names
                .first()
                .ok_or_else(|| DbError::Parse("DROP COLUMN missing column name".into()))?
                .value
                .clone();
            Ok(AlterTableAction::DropColumn(name))
        }

        sq::AlterTableOperation::RenameColumn {
            old_column_name,
            new_column_name,
        } => Ok(AlterTableAction::RenameColumn {
            old: old_column_name.value.clone(),
            new: new_column_name.value.clone(),
        }),

        sq::AlterTableOperation::RenameTable { table_name } => {
            let name = match table_name {
                sq::RenameTableNameKind::To(n) => n.to_string(),
                sq::RenameTableNameKind::As(n) => n.to_string(),
            };
            Ok(AlterTableAction::RenameTable(name))
        }

        sq::AlterTableOperation::AlterColumn { column_name, op } => match op {
            sq::AlterColumnOperation::SetDataType { data_type, .. } => {
                Ok(AlterTableAction::AlterColumnType {
                    column: column_name.value.clone(),
                    new_type: translate_data_type(data_type)?,
                })
            }
            other => Err(DbError::Parse(format!(
                "unsupported ALTER COLUMN operation: {:?}",
                other
            ))),
        },

        sq::AlterTableOperation::AddConstraint { constraint, .. } => match constraint {
            sq::TableConstraint::ForeignKey(fk) => {
                Ok(AlterTableAction::AddForeignKey(translate_fk_constraint(fk)))
            }
            sq::TableConstraint::PrimaryKey(pk) => Ok(AlterTableAction::AddPrimaryKey(
                pk.columns.iter().map(|c| c.column.to_string()).collect(),
            )),
            other => Err(DbError::Parse(format!(
                "unsupported ADD CONSTRAINT type: {:?}",
                other
            ))),
        },

        sq::AlterTableOperation::DropConstraint { name, .. } => {
            Ok(AlterTableAction::DropConstraint(name.value.clone()))
        }

        sq::AlterTableOperation::DropForeignKey { name, .. } => {
            Ok(AlterTableAction::DropForeignKey(name.value.clone()))
        }

        other => Err(DbError::Parse(format!(
            "unsupported ALTER TABLE operation: {:?}",
            other
        ))),
    }
}

fn translate_insert(insert: sq::Insert) -> Result<Statement> {
    let table = {
        let t = insert.table.to_string();
        if t.trim().is_empty() {
            return Err(DbError::Parse("INSERT requires a table name".into()));
        }
        t
    };
    let columns = if insert.columns.is_empty() {
        None
    } else {
        Some(insert.columns.iter().map(|c| c.value.clone()).collect())
    };

    let source = match insert.source {
        Some(q) => match *q.body {
            sq::SetExpr::Values(values) => {
                let rows = values
                    .rows
                    .iter()
                    .map(|row| row.iter().map(translate_expr).collect::<Result<Vec<_>>>())
                    .collect::<Result<Vec<_>>>()?;
                InsertSource::Values(rows)
            }
            sq::SetExpr::Select(_) | sq::SetExpr::Query(_) => {
                let sel = translate_query(&q)?;
                InsertSource::Select(Box::new(sel))
            }
            other => {
                return Err(DbError::Parse(format!(
                    "unsupported INSERT source: {:?}",
                    other
                )));
            }
        },
        None => return Err(DbError::Parse("INSERT with no source".to_string())),
    };

    Ok(Statement::Insert(InsertStmt {
        table,
        columns,
        source,
    }))
}

fn translate_update(
    table: sq::TableWithJoins,
    assignments: Vec<sq::Assignment>,
    selection: Option<sq::Expr>,
) -> Result<Statement> {
    let (name, alias) = match &table.relation {
        sq::TableFactor::Table { name, alias, .. } => (
            name.to_string(),
            alias.as_ref().map(|a| a.name.value.clone()),
        ),
        other => {
            return Err(DbError::Parse(format!(
                "UPDATE with non-table relation: {:?}",
                other
            )));
        }
    };

    let assignments = assignments
        .into_iter()
        .map(|a| {
            let col = match a.target {
                sq::AssignmentTarget::ColumnName(obj_name) => {
                    obj_name.0.last().map(|p| p.to_string()).unwrap_or_default()
                }
                sq::AssignmentTarget::Tuple(_) => {
                    return Err(DbError::Parse(
                        "tuple assignment target is not supported".to_string(),
                    ));
                }
            };
            Ok(Assignment {
                column: col,
                value: translate_expr(&a.value)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let where_clause = selection.as_ref().map(translate_expr).transpose()?;

    Ok(Statement::Update(UpdateStmt {
        table: name,
        alias,
        assignments,
        where_clause,
    }))
}

fn translate_delete(delete: sq::Delete) -> Result<Statement> {
    if delete.tables.len() > 1 {
        return Err(DbError::Parse(
            "multi-table DELETE is not supported".to_string(),
        ));
    }

    let from_tables: Vec<&sq::TableWithJoins> = match &delete.from {
        sq::FromTable::WithFromKeyword(tables) => tables.iter().collect(),
        sq::FromTable::WithoutKeyword(tables) => tables.iter().collect(),
    };

    let (name, alias) = if let Some(from) = from_tables.first() {
        match &from.relation {
            sq::TableFactor::Table { name, alias, .. } => (
                name.to_string(),
                alias.as_ref().map(|a| a.name.value.clone()),
            ),
            other => {
                return Err(DbError::Parse(format!(
                    "DELETE with non-table relation: {:?}",
                    other
                )));
            }
        }
    } else {
        return Err(DbError::Parse("DELETE with no FROM clause".to_string()));
    };

    let where_clause = delete.selection.as_ref().map(translate_expr).transpose()?;

    Ok(Statement::Delete(DeleteStmt {
        table: name,
        alias,
        where_clause,
    }))
}

fn translate_query(q: &sq::Query) -> Result<SelectStmt> {
    match q.body.as_ref() {
        sq::SetExpr::Select(sel) => translate_select(sel, q),
        sq::SetExpr::Query(inner) => translate_query(inner),
        other => Err(DbError::Parse(format!(
            "unsupported query body: {:?}",
            other
        ))),
    }
}

fn translate_select(sel: &sq::Select, q: &sq::Query) -> Result<SelectStmt> {
    let distinct = matches!(sel.distinct, Some(sq::Distinct::Distinct));

    let projections = sel
        .projection
        .iter()
        .map(translate_select_item)
        .collect::<Result<Vec<_>>>()?;

    let mut from: Vec<TableRef> = Vec::new();
    let mut joins: Vec<Join> = Vec::new();

    for table_with_joins in &sel.from {
        from.push(translate_table_factor(&table_with_joins.relation)?);
        for join in &table_with_joins.joins {
            joins.push(translate_join(join)?);
        }
    }

    let where_clause = sel.selection.as_ref().map(translate_expr).transpose()?;

    let group_by = match &sel.group_by {
        sq::GroupByExpr::Expressions(exprs, _) => exprs
            .iter()
            .map(translate_expr)
            .collect::<Result<Vec<_>>>()?,
        sq::GroupByExpr::All(_) => {
            return Err(DbError::Parse("GROUP BY ALL is not supported".to_string()));
        }
    };

    let having = sel.having.as_ref().map(translate_expr).transpose()?;

    let order_by = q
        .order_by
        .as_ref()
        .map(|ob| -> Result<Vec<OrderByExpr>> {
            match &ob.kind {
                sq::OrderByKind::Expressions(exprs) => exprs
                    .iter()
                    .map(|o| {
                        Ok(OrderByExpr {
                            expr: translate_expr(&o.expr)?,
                            asc: !matches!(o.options.asc, Some(false)),
                            nulls_first: o.options.nulls_first,
                        })
                    })
                    .collect(),
                sq::OrderByKind::All(_) => {
                    Err(DbError::Parse("ORDER BY ALL is not supported".to_string()))
                }
            }
        })
        .transpose()?
        .unwrap_or_default();

    let (limit, offset) = match q.limit_clause.as_ref() {
        Some(sq::LimitClause::LimitOffset { limit, offset, .. }) => {
            let l = limit.as_ref().map(translate_expr).transpose()?;
            let o = offset
                .as_ref()
                .map(|off| translate_expr(&off.value))
                .transpose()?;
            (l, o)
        }
        Some(sq::LimitClause::OffsetCommaLimit { offset, limit }) => {
            (Some(translate_expr(limit)?), Some(translate_expr(offset)?))
        }
        None => (None, None),
    };

    Ok(SelectStmt {
        projections,
        from,
        joins,
        where_clause,
        group_by,
        having,
        order_by,
        limit,
        offset,
        distinct,
    })
}

fn translate_select_item(item: &sq::SelectItem) -> Result<SelectItem> {
    match item {
        sq::SelectItem::Wildcard(_) => Ok(SelectItem::Wildcard),
        sq::SelectItem::QualifiedWildcard(name, _) => {
            Ok(SelectItem::QualifiedWildcard(name.to_string()))
        }
        sq::SelectItem::UnnamedExpr(e) => Ok(SelectItem::Expr {
            expr: translate_expr(e)?,
            alias: None,
        }),
        sq::SelectItem::ExprWithAlias { expr, alias } => Ok(SelectItem::Expr {
            expr: translate_expr(expr)?,
            alias: Some(alias.value.clone()),
        }),
    }
}

fn translate_table_factor(factor: &sq::TableFactor) -> Result<TableRef> {
    match factor {
        sq::TableFactor::Table { name, alias, .. } => Ok(TableRef::Named {
            name: name.to_string(),
            alias: alias.as_ref().map(|a| a.name.value.clone()),
        }),
        sq::TableFactor::Derived {
            subquery, alias, ..
        } => {
            let alias = alias
                .as_ref()
                .map(|a| a.name.value.clone())
                .ok_or_else(|| DbError::Parse("subquery in FROM must have an alias".to_string()))?;
            Ok(TableRef::Subquery {
                query: Box::new(translate_query(subquery)?),
                alias,
            })
        }
        other => Err(DbError::Parse(format!(
            "unsupported table factor: {:?}",
            other
        ))),
    }
}

fn translate_join(join: &sq::Join) -> Result<Join> {
    let table = translate_table_factor(&join.relation)?;

    let (kind, condition) = match &join.join_operator {
        sq::JoinOperator::Inner(constraint) => {
            (JoinKind::Inner, translate_join_constraint(constraint)?)
        }
        sq::JoinOperator::LeftOuter(constraint) => {
            (JoinKind::LeftOuter, translate_join_constraint(constraint)?)
        }
        sq::JoinOperator::RightOuter(constraint) => {
            (JoinKind::RightOuter, translate_join_constraint(constraint)?)
        }
        sq::JoinOperator::FullOuter(constraint) => {
            (JoinKind::FullOuter, translate_join_constraint(constraint)?)
        }
        sq::JoinOperator::CrossJoin(_) => (JoinKind::Cross, JoinCondition::None),
        sq::JoinOperator::CrossApply => (JoinKind::Cross, JoinCondition::None),
        other => {
            return Err(DbError::Parse(format!(
                "unsupported join type: {:?}",
                other
            )));
        }
    };

    Ok(Join {
        kind,
        table,
        condition,
    })
}

fn translate_join_constraint(constraint: &sq::JoinConstraint) -> Result<JoinCondition> {
    match constraint {
        sq::JoinConstraint::On(expr) => Ok(JoinCondition::On(translate_expr(expr)?)),
        sq::JoinConstraint::Using(cols) => Ok(JoinCondition::Using(
            cols.iter().map(|c| c.to_string()).collect(),
        )),
        sq::JoinConstraint::Natural => Ok(JoinCondition::Natural),
        sq::JoinConstraint::None => Ok(JoinCondition::None),
    }
}

fn translate_expr(expr: &sq::Expr) -> Result<Expr> {
    match expr {
        sq::Expr::Value(v) => Ok(Expr::Literal(translate_value(&v.value)?)),

        sq::Expr::Identifier(id) => Ok(Expr::Column(ColumnRef {
            table: None,
            column: id.value.clone(),
        })),

        sq::Expr::CompoundIdentifier(parts) if parts.len() == 2 => Ok(Expr::Column(ColumnRef {
            table: Some(parts[0].value.clone()),
            column: parts[1].value.clone(),
        })),

        sq::Expr::CompoundIdentifier(parts) if parts.len() > 2 => Ok(Expr::Column(ColumnRef {
            table: Some(parts[parts.len() - 2].value.clone()),
            column: parts[parts.len() - 1].value.clone(),
        })),

        sq::Expr::BinaryOp { left, op, right } => Ok(Expr::BinaryOp {
            left: Box::new(translate_expr(left)?),
            op: translate_binary_op(op)?,
            right: Box::new(translate_expr(right)?),
        }),

        sq::Expr::UnaryOp { op, expr } => {
            let uop = match op {
                sq::UnaryOperator::Minus => UnaryOp::Neg,
                sq::UnaryOperator::Not => UnaryOp::Not,
                sq::UnaryOperator::Plus => return translate_expr(expr),
                other => {
                    return Err(DbError::Parse(format!(
                        "unsupported unary operator: {:?}",
                        other
                    )));
                }
            };
            Ok(Expr::UnaryOp {
                op: uop,
                expr: Box::new(translate_expr(expr)?),
            })
        }

        sq::Expr::IsNull(e) => Ok(Expr::IsNull(Box::new(translate_expr(e)?))),
        sq::Expr::IsNotNull(e) => Ok(Expr::IsNotNull(Box::new(translate_expr(e)?))),

        sq::Expr::Like {
            expr,
            pattern,
            negated,
            ..
        } => Ok(Expr::Like {
            expr: Box::new(translate_expr(expr)?),
            pattern: Box::new(translate_expr(pattern)?),
            negated: *negated,
        }),

        sq::Expr::ILike {
            expr,
            pattern,
            negated,
            ..
        } => Ok(Expr::Like {
            expr: Box::new(translate_expr(expr)?),
            pattern: Box::new(translate_expr(pattern)?),
            negated: *negated,
        }),

        sq::Expr::Between {
            expr,
            low,
            high,
            negated,
        } => Ok(Expr::Between {
            expr: Box::new(translate_expr(expr)?),
            low: Box::new(translate_expr(low)?),
            high: Box::new(translate_expr(high)?),
            negated: *negated,
        }),

        sq::Expr::InList {
            expr,
            list,
            negated,
        } => {
            let list = list
                .iter()
                .map(translate_expr)
                .collect::<Result<Vec<_>>>()?;
            Ok(Expr::In {
                expr: Box::new(translate_expr(expr)?),
                list,
                negated: *negated,
            })
        }

        sq::Expr::InSubquery {
            expr,
            subquery,
            negated,
        } => Ok(Expr::InSubquery {
            expr: Box::new(translate_expr(expr)?),
            subquery: Box::new(translate_query(subquery)?),
            negated: *negated,
        }),

        sq::Expr::Exists { subquery, negated } => Ok(Expr::Exists {
            subquery: Box::new(translate_query(subquery)?),
            negated: *negated,
        }),

        sq::Expr::Subquery(q) => Ok(Expr::Subquery(Box::new(translate_query(q)?))),

        sq::Expr::Function(f) => {
            let name = f.name.to_string().to_lowercase();
            let distinct = matches!(
                &f.args,
                sq::FunctionArguments::List(list)
                    if matches!(
                        list.duplicate_treatment,
                        Some(sq::DuplicateTreatment::Distinct)
                    )
            );
            let args = match &f.args {
                sq::FunctionArguments::List(list) => list
                    .args
                    .iter()
                    .map(|a| match a {
                        sq::FunctionArg::Unnamed(sq::FunctionArgExpr::Expr(e)) => translate_expr(e),
                        sq::FunctionArg::Unnamed(sq::FunctionArgExpr::Wildcard) => {
                            Ok(Expr::Wildcard)
                        }
                        sq::FunctionArg::Named {
                            arg: sq::FunctionArgExpr::Expr(e),
                            ..
                        } => translate_expr(e),
                        other => Err(DbError::Parse(format!(
                            "unsupported function argument: {:?}",
                            other
                        ))),
                    })
                    .collect::<Result<Vec<_>>>()?,
                sq::FunctionArguments::None => vec![],
                sq::FunctionArguments::Subquery(q) => {
                    vec![Expr::Subquery(Box::new(translate_query(q)?))]
                }
            };
            Ok(Expr::Function {
                name,
                args,
                distinct,
            })
        }

        sq::Expr::Cast {
            expr, data_type, ..
        } => Ok(Expr::Cast {
            expr: Box::new(translate_expr(expr)?),
            data_type: translate_data_type(data_type)?,
        }),

        sq::Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            let when_then = conditions
                .iter()
                .map(|cw| Ok((translate_expr(&cw.condition)?, translate_expr(&cw.result)?)))
                .collect::<Result<Vec<_>>>()?;
            Ok(Expr::Case {
                operand: operand
                    .as_ref()
                    .map(|o| translate_expr(o).map(Box::new))
                    .transpose()?,
                when_then,
                else_result: else_result
                    .as_ref()
                    .map(|e| translate_expr(e).map(Box::new))
                    .transpose()?,
            })
        }

        sq::Expr::Wildcard(_) => Ok(Expr::Wildcard),

        sq::Expr::Nested(e) => translate_expr(e),

        other => Err(DbError::Parse(format!(
            "unsupported expression: {:?}",
            other
        ))),
    }
}

fn translate_value(v: &sq::Value) -> Result<Literal> {
    match v {
        sq::Value::Number(n, _) => {
            if let Ok(i) = n.parse::<i64>() {
                Ok(Literal::Integer(i))
            } else if let Ok(f) = n.parse::<f64>() {
                Ok(Literal::Float(f))
            } else {
                Err(DbError::Parse(format!("invalid number literal: {}", n)))
            }
        }
        sq::Value::SingleQuotedString(s) | sq::Value::DoubleQuotedString(s) => {
            Ok(Literal::String(s.clone()))
        }
        sq::Value::Boolean(b) => Ok(Literal::Bool(*b)),
        sq::Value::Null => Ok(Literal::Null),
        other => Err(DbError::Parse(format!(
            "unsupported literal value: {:?}",
            other
        ))),
    }
}

fn translate_binary_op(op: &sq::BinaryOperator) -> Result<BinaryOp> {
    match op {
        sq::BinaryOperator::Plus => Ok(BinaryOp::Add),
        sq::BinaryOperator::Minus => Ok(BinaryOp::Sub),
        sq::BinaryOperator::Multiply => Ok(BinaryOp::Mul),
        sq::BinaryOperator::Divide => Ok(BinaryOp::Div),
        sq::BinaryOperator::Modulo => Ok(BinaryOp::Mod),
        sq::BinaryOperator::Eq => Ok(BinaryOp::Eq),
        sq::BinaryOperator::NotEq => Ok(BinaryOp::NotEq),
        sq::BinaryOperator::Lt => Ok(BinaryOp::Lt),
        sq::BinaryOperator::LtEq => Ok(BinaryOp::LtEq),
        sq::BinaryOperator::Gt => Ok(BinaryOp::Gt),
        sq::BinaryOperator::GtEq => Ok(BinaryOp::GtEq),
        sq::BinaryOperator::And => Ok(BinaryOp::And),
        sq::BinaryOperator::Or => Ok(BinaryOp::Or),
        sq::BinaryOperator::StringConcat => Ok(BinaryOp::Concat),
        other => Err(DbError::Parse(format!(
            "unsupported binary operator: {:?}",
            other
        ))),
    }
}

pub fn translate_data_type(dt: &sq::DataType) -> Result<DataType> {
    match dt {
        sq::DataType::TinyInt(_) => Ok(DataType::I8),
        sq::DataType::SmallInt(_) | sq::DataType::Int2(_) => Ok(DataType::I16),
        sq::DataType::Int(_) | sq::DataType::Integer(_) | sq::DataType::Int4(_) => {
            Ok(DataType::I32)
        }
        sq::DataType::BigInt(_) | sq::DataType::Int8(_) => Ok(DataType::I64),

        sq::DataType::Unsigned | sq::DataType::UnsignedInteger => Ok(DataType::U32),

        sq::DataType::Float(_) | sq::DataType::Real => Ok(DataType::F32),
        sq::DataType::Double(_) | sq::DataType::DoublePrecision => Ok(DataType::F64),

        sq::DataType::Boolean | sq::DataType::Bool => Ok(DataType::Bool),

        sq::DataType::Varchar(_)
        | sq::DataType::Char(_)
        | sq::DataType::CharVarying(_)
        | sq::DataType::CharacterVarying(_)
        | sq::DataType::Text
        | sq::DataType::String(_)
        | sq::DataType::Clob(_) => Ok(DataType::String),

        sq::DataType::Custom(name, _) => match name.to_string().to_lowercase().as_str() {
            "text" | "string" | "varchar" | "char" => Ok(DataType::String),
            "bool" | "boolean" => Ok(DataType::Bool),
            "int" | "integer" | "int4" => Ok(DataType::I32),
            "bigint" | "int8" => Ok(DataType::I64),
            "smallint" | "int2" => Ok(DataType::I16),
            "float" | "real" | "float4" => Ok(DataType::F32),
            "double" | "float8" => Ok(DataType::F64),
            other => Err(DbError::Parse(format!(
                "unsupported custom data type: {}",
                other
            ))),
        },

        other => Err(DbError::Parse(format!(
            "unsupported data type: {:?}",
            other
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_start_transaction() {
        let valid_stmts = [
            "BEGIN;",
            "BEGIN TRANSACTION;",
            "BEGIN WORK;",
            "START TRANSACTION;",
            "BEGIN ISOLATION LEVEL READ COMMITTED;",
            "BEGIN ISOLATION LEVEL SERIALIZABLE;",
        ];

        for sql in &valid_stmts {
            let res = Parser::parse(sql);
            assert!(res.is_ok(), "Expected valid SQL, got error: {:?}, sql: {}", res, sql);
        }
    }

    #[test]
    fn test_invalid_start_transaction() {
        let invalid_stmts = [
            "BEGIN SELECT 1 END;",
            "BEGIN EXCEPTION WHEN ERROR THEN SELECT 2 END;",
            "BEGIN TRANSACTION ISOLATION LEVEL READ COMMITTED ISOLATION LEVEL SERIALIZABLE;",
            "BEGIN ISOLATION LEVEL SERIALIZABLE READ ONLY;",
        ];

        for sql in &invalid_stmts {
            let res = Parser::parse(sql);
            assert!(res.is_err(), "Expected parse error, got: {:?}, sql: {}", res, sql);
        }
    }
}
