use std::sync::Arc;

use crate::{
    accessor::Accessor,
    catalog,
    common::txn::Txn,
    databox::DataType,
    parser::ast::{
        AlterTableAction, AlterTableStmt, BinaryOp, CreateIndexStmt, CreateTableStmt, DeleteStmt,
        DropIndexStmt, DropTableStmt, Expr, InsertSource, InsertStmt, JoinCondition, JoinKind,
        Literal, SelectItem, SelectStmt, Statement, TableRef, UnaryOp, UpdateStmt,
    },
};

use super::{
    bound::*,
    error::{BindError, BindResult},
    oid_alloc::OidAllocator,
    scope::Scope,
};

pub struct Binder<A: Accessor, O: OidAllocator> {
    accessor: Arc<A>,
    oids: O,
    txn: Txn,
}

impl<A: Accessor, O: OidAllocator> Binder<A, O> {
    pub fn new(accessor: Arc<A>, oids: O, txn: Txn) -> Self {
        Self {
            accessor,
            oids,
            txn,
        }
    }

    pub fn bind(&self, stmt: Statement) -> BindResult<BoundStatement> {
        match stmt {
            // DDL: Table
            Statement::CreateTable(s)   => self.bind_create_table(s),
            Statement::DropTable(s)     => self.bind_drop_table(s),
            Statement::AlterTable(s)    => self.bind_alter_table(s),
            
             // DDL: Index
            Statement::CreateIndex(s) => self.bind_create_index(s),
            Statement::DropIndex(s)   => self.bind_drop_index(s),

            // DML
            Statement::Insert(s) => self.bind_insert(s),
            Statement::Select(s) => Ok(BoundStatement::Select(self.bind_select(s)?)),
            Statement::Update(s) => self.bind_update(s),
            Statement::Delete(s) => self.bind_delete(s),

            // Misc
             Statement::Explain(inner) => Ok(BoundStatement::Explain(Box::new(self.bind(*inner)?))),

            // These are handled in Session::execute_sql and never passed to the binder.
            Statement::BeginTransaction(_)  => unreachable!(),
            Statement::Commit               => unreachable!(),
            Statement::Rollback             => unreachable!(),
        }
    }

    fn bind_create_table(&self, stmt: CreateTableStmt) -> BindResult<BoundStatement> {
        let exists = self
            .accessor
            .catalog_get_table_by_name(self.txn, &stmt.name)
            .is_ok();

        if exists {
            if stmt.if_not_exists {
            } else {
                return Err(BindError::TableAlreadyExists(stmt.name));
            }
        }

        let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();
        for col in &stmt.columns {
            let lower = col.name.to_lowercase();
            if !seen_names.insert(lower) {
                return Err(BindError::DuplicateName(col.name.clone()));
            }
        }

        let table_oid = self.oids.next_oid();
        let file_id = self.oids.next_file_id();

        let mut bound_cols: Vec<BoundColumnDef> = Vec::with_capacity(stmt.columns.len());
        for (pos, col) in stmt.columns.iter().enumerate() {
            let default = col
                .default
                .as_ref()
                .map(|e| self.bind_default_expr(e))
                .transpose()?;
            bound_cols.push(BoundColumnDef {
                oid: self.oids.next_oid(),
                name: col.name.clone(),
                data_type: col.data_type,
                position: pos as u16,
                nullable: col.nullable,
                default,
                unique: col.unique,
                primary_key: col.primary_key,
            });
        }

        let mut pk_positions: Vec<u16> = Vec::new();
        for pk_name in &stmt.primary_key {
            let pos = bound_cols
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(pk_name))
                .map(|c| c.position)
                .ok_or_else(|| BindError::UnknownColumn(pk_name.clone()))?;
            pk_positions.push(pos);
        }

        Ok(BoundStatement::CreateTable(BoundCreateTable {
            table_oid,
            file_id,
            name: stmt.name,
            columns: bound_cols,
            primary_key: pk_positions,
            foreign_keys: stmt.foreign_keys,
            if_not_exists: stmt.if_not_exists,
        }))
    }

    fn bind_drop_table(&self, stmt: DropTableStmt) -> BindResult<BoundStatement> {
        match self
            .accessor
            .catalog_get_table_by_name(self.txn, &stmt.name)
        {
            Ok(table) => Ok(BoundStatement::DropTable(BoundDropTable {
                table_oid: table.oid,
                name: stmt.name,
                index_oids: vec![],
                if_exists: stmt.if_exists,
            })),
            Err(_) if stmt.if_exists => Ok(BoundStatement::DropTable(BoundDropTable {
                table_oid: 0,
                name: stmt.name,
                index_oids: vec![],
                if_exists: true,
            })),
            Err(_) => Err(BindError::UnknownTable(stmt.name)),
        }
    }

    fn bind_alter_table(&self, stmt: AlterTableStmt) -> BindResult<BoundStatement> {
        let table = self
            .accessor
            .catalog_get_table_by_name(self.txn, &stmt.name)
            .map_err(|_| BindError::UnknownTable(stmt.name.clone()))?;

        let columns = self
            .accessor
            .catalog_get_table_columns(self.txn, table.oid)
            .map_err(|e| BindError::CatalogError(format!("{e:?}")))?;

        let action = match stmt.action {
            AlterTableAction::AddColumn(col_def) => {
                let position = columns.len() as u16;
                let default = col_def
                    .default
                    .as_ref()
                    .map(|e| self.bind_default_expr(e))
                    .transpose()?;
                BoundAlterAction::AddColumn(BoundColumnDef {
                    oid: self.oids.next_oid(),
                    name: col_def.name.clone(),
                    data_type: col_def.data_type,
                    position,
                    nullable: col_def.nullable,
                    default,
                    unique: col_def.unique,
                    primary_key: col_def.primary_key,
                })
            }
            AlterTableAction::DropColumn(col_name) => {
                let col = columns
                    .iter()
                    .find(|c| c.name.eq_ignore_ascii_case(&col_name))
                    .ok_or_else(|| BindError::UnknownColumn(col_name))?;
                BoundAlterAction::DropColumn {
                    column_oid: col.oid,
                    position: col.position,
                }
            }
            AlterTableAction::RenameColumn { old, new } => {
                let col = columns
                    .iter()
                    .find(|c| c.name.eq_ignore_ascii_case(&old))
                    .ok_or_else(|| BindError::UnknownColumn(old.clone()))?;
                BoundAlterAction::RenameColumn {
                    column_oid: col.oid,
                    old_name: old,
                    new_name: new,
                }
            }
            AlterTableAction::RenameTable(new_name) => BoundAlterAction::RenameTable { new_name },
            AlterTableAction::AlterColumnType { column, new_type } => {
                let col = columns
                    .iter()
                    .find(|c| c.name.eq_ignore_ascii_case(&column))
                    .ok_or_else(|| BindError::UnknownColumn(column))?;
                BoundAlterAction::AlterColumnType {
                    column_oid: col.oid,
                    new_type,
                }
            }
            AlterTableAction::AddForeignKey(fk) => BoundAlterAction::AddForeignKey(fk),
            AlterTableAction::DropForeignKey(name) => BoundAlterAction::DropForeignKey(name),
            AlterTableAction::AddPrimaryKey(col_names) => {
                let mut oids = Vec::with_capacity(col_names.len());
                for name in &col_names {
                    let col = columns
                        .iter()
                        .find(|c| c.name.eq_ignore_ascii_case(name))
                        .ok_or_else(|| BindError::UnknownColumn(name.clone()))?;
                    oids.push(col.oid);
                }
                BoundAlterAction::AddPrimaryKey(oids)
            }
            AlterTableAction::DropConstraint(name) => BoundAlterAction::DropConstraint(name),
        };

        Ok(BoundStatement::AlterTable(BoundAlterTable {
            table_oid: table.oid,
            name: stmt.name,
            action,
        }))
    }

    fn bind_create_index(&self, stmt: CreateIndexStmt) -> BindResult<BoundStatement> {
        if stmt.columns.len() > 1 {
            return Err(BindError::MultiColumnIndexUnsupported);
        }
        if stmt.columns.is_empty() {
            return Err(BindError::NoColumnsInIndex);
        }

        let exists = self
            .accessor
            .catalog_get_index_by_name(self.txn, &stmt.name)
            .is_ok();

        if exists {
            if stmt.if_not_exists {
            } else {
                return Err(BindError::IndexAlreadyExists(stmt.name));
            }
        }

        let table = self
            .accessor
            .catalog_get_table_by_name(self.txn, &stmt.table)
            .map_err(|_| BindError::UnknownTable(stmt.table.clone()))?;

        let columns = self
            .accessor
            .catalog_get_table_columns(self.txn, table.oid)
            .map_err(|e| BindError::CatalogError(format!("{e:?}")))?;

        let col_name = &stmt.columns[0];
        let col = columns
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(col_name))
            .ok_or_else(|| BindError::UnknownColumn(col_name.clone()))?;

        Ok(BoundStatement::CreateIndex(BoundCreateIndex {
            index_oid: self.oids.next_oid(),
            file_id: self.oids.next_file_id(),
            name: stmt.name,
            table_oid: table.oid,
            column_oid: col.oid,
            unique: stmt.unique,
            if_not_exists: stmt.if_not_exists,
        }))
    }

    fn bind_drop_index(&self, stmt: DropIndexStmt) -> BindResult<BoundStatement> {
        match self
            .accessor
            .catalog_get_index_by_name(self.txn, &stmt.name)
        {
            Ok(index) => Ok(BoundStatement::DropIndex(BoundDropIndex {
                index_oid: index.oid,
                name: stmt.name,
                table_oid: index.table_oid,
                if_exists: stmt.if_exists,
            })),
            Err(_) if stmt.if_exists => Ok(BoundStatement::DropIndex(BoundDropIndex {
                index_oid: 0,
                name: stmt.name,
                table_oid: 0,
                if_exists: true,
            })),
            Err(_) => Err(BindError::UnknownTable(stmt.name)),
        }
    }

    fn bind_insert(&self, stmt: InsertStmt) -> BindResult<BoundStatement> {
        let table = self
            .accessor
            .catalog_get_table_by_name(self.txn, &stmt.table)
            .map_err(|_| BindError::UnknownTable(stmt.table.clone()))?;

        let mut table_columns = self
            .accessor
            .catalog_get_table_columns(self.txn, table.oid)
            .map_err(|e| BindError::CatalogError(format!("{e:?}")))?;

        table_columns.sort_by_key(|c| c.position);

        let target_columns: Vec<catalog::Column> = match &stmt.columns {
            None => table_columns.clone(),
            Some(names) => {
                let mut resolved = Vec::with_capacity(names.len());
                for name in names {
                    let col = table_columns
                        .iter()
                        .find(|c| c.name.eq_ignore_ascii_case(name))
                        .ok_or_else(|| BindError::UnknownColumn(name.clone()))?
                        .clone();
                    resolved.push(col);
                }
                resolved
            }
        };

        let source = match stmt.source {
            InsertSource::Values(rows) => {
                let mut bound_rows = Vec::with_capacity(rows.len());
                for row in rows {
                    if row.len() != target_columns.len() {
                        return Err(BindError::ColumnCountMismatch {
                            expected: target_columns.len(),
                            found: row.len(),
                        });
                    }
                    let scope = Scope::new();
                    let mut bound_row = Vec::with_capacity(row.len());
                    for (expr, col) in row.iter().zip(target_columns.iter()) {
                        let bound = self.bind_expr(expr, &scope)?;
                        let coerced =
                            self.coerce_expr(bound, col.type_id, &col.name.to_string())?;
                        bound_row.push(coerced);
                    }
                    bound_rows.push(bound_row);
                }
                BoundInsertSource::Values(bound_rows)
            }
            InsertSource::Select(sel) => {
                let bound_sel = self.bind_select(*sel)?;
                if bound_sel.output_columns.len() != target_columns.len() {
                    return Err(BindError::ColumnCountMismatch {
                        expected: target_columns.len(),
                        found: bound_sel.output_columns.len(),
                    });
                }
                BoundInsertSource::Select(Box::new(bound_sel))
            }
        };

        Ok(BoundStatement::Insert(BoundInsert {
            table,
            table_columns,
            target_columns,
            source,
        }))
    }

    fn bind_update(&self, stmt: UpdateStmt) -> BindResult<BoundStatement> {
        let table = self
            .accessor
            .catalog_get_table_by_name(self.txn, &stmt.table)
            .map_err(|_| BindError::UnknownTable(stmt.table.clone()))?;

        let mut table_columns = self
            .accessor
            .catalog_get_table_columns(self.txn, table.oid)
            .map_err(|e| BindError::CatalogError(format!("{e:?}")))?;
        table_columns.sort_by_key(|c| c.position);

        let qualifier = stmt.alias.as_deref().unwrap_or(&stmt.table);
        let mut scope = Scope::new();
        scope.add_table(Some(qualifier.to_string()), &table_columns, false);

        let mut assignments = Vec::with_capacity(stmt.assignments.len());
        for asgn in stmt.assignments {
            let col = table_columns
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(&asgn.column))
                .ok_or_else(|| BindError::UnknownColumn(asgn.column.clone()))?
                .clone();
            let value = self.bind_expr(&asgn.value, &scope)?;
            let value = self.coerce_expr(value, col.type_id, &col.name.to_string())?;
            assignments.push(BoundAssignment { column: col, value });
        }

        let where_clause = stmt
            .where_clause
            .as_ref()
            .map(|e| self.bind_expr(e, &scope))
            .transpose()?;

        if let Some(ref wc) = where_clause {
            self.expect_bool_or_numeric(wc, "WHERE")?;
        }

        Ok(BoundStatement::Update(BoundUpdate {
            table,
            table_columns,
            assignments,
            where_clause,
        }))
    }

    fn bind_delete(&self, stmt: DeleteStmt) -> BindResult<BoundStatement> {
        let table = self
            .accessor
            .catalog_get_table_by_name(self.txn, &stmt.table)
            .map_err(|_| BindError::UnknownTable(stmt.table.clone()))?;

        let mut table_columns = self
            .accessor
            .catalog_get_table_columns(self.txn, table.oid)
            .map_err(|e| BindError::CatalogError(format!("{e:?}")))?;
        table_columns.sort_by_key(|c| c.position);

        let qualifier = stmt.alias.as_deref().unwrap_or(&stmt.table);
        let mut scope = Scope::new();
        scope.add_table(Some(qualifier.to_string()), &table_columns, false);

        let where_clause = stmt
            .where_clause
            .as_ref()
            .map(|e| self.bind_expr(e, &scope))
            .transpose()?;

        if let Some(ref wc) = where_clause {
            self.expect_bool_or_numeric(wc, "WHERE")?;
        }

        Ok(BoundStatement::Delete(BoundDelete {
            table,
            table_columns,
            where_clause,
        }))
    }

    fn bind_select(&self, stmt: SelectStmt) -> BindResult<BoundSelect> {
        let mut scope = Scope::new();
        let mut bound_from: Vec<BoundTableRef> = Vec::new();

        for table_ref in stmt.from {
            let bound_ref = self.bind_table_ref(table_ref, false)?;
            self.add_table_ref_to_scope(&bound_ref, &mut scope, false);
            bound_from.push(bound_ref);
        }

        let mut bound_joins: Vec<BoundJoin> = Vec::new();
        for join in stmt.joins {
            let is_right_or_full = matches!(join.kind, JoinKind::RightOuter | JoinKind::FullOuter);
            let is_left_or_full = matches!(join.kind, JoinKind::LeftOuter | JoinKind::FullOuter);
            let bound_ref = self.bind_table_ref(join.table, is_right_or_full)?;

            self.add_table_ref_to_scope(
                &bound_ref,
                &mut scope,
                is_left_or_full || is_right_or_full,
            );

            let join_kind = join.kind.clone();
            let condition =
                self.bind_join_condition(join.condition, join.kind, &scope, &bound_ref)?;

            bound_joins.push(BoundJoin {
                kind: translate_join_kind(join.kind),
                table: bound_ref,
                condition,
            });
        }

        let where_clause = stmt
            .where_clause
            .as_ref()
            .map(|e| self.bind_expr(e, &scope))
            .transpose()?;

        let group_by = stmt
            .group_by
            .iter()
            .map(|e| self.bind_expr(e, &scope))
            .collect::<BindResult<Vec<_>>>()?;

        let having = stmt
            .having
            .as_ref()
            .map(|e| self.bind_expr(e, &scope))
            .transpose()?;

        let mut projections: Vec<BoundSelectItem> = Vec::new();
        for item in stmt.projections {
            match item {
                SelectItem::Wildcard => {
                    for sc in &scope.columns {
                        projections.push(BoundSelectItem {
                            alias: sc.column_name.clone(),
                            expr: BoundExpr {
                                kind: BoundExprKind::ColumnRef(BoundColumnRef {
                                    qualifier: sc.qualifier.clone(),
                                    column_name: sc.column_name.clone(),
                                    scope_index: sc.index,
                                }),
                                data_type: sc.data_type,
                                nullable: sc.nullable,
                            },
                        });
                    }
                }
                SelectItem::QualifiedWildcard(qualifier) => {
                    let cols = scope.all_columns_for_qualifier(&qualifier);
                    if cols.is_empty() {
                        return Err(BindError::UnknownTable(qualifier));
                    }
                    for sc in cols {
                        projections.push(BoundSelectItem {
                            alias: sc.column_name.clone(),
                            expr: BoundExpr {
                                kind: BoundExprKind::ColumnRef(BoundColumnRef {
                                    qualifier: sc.qualifier.clone(),
                                    column_name: sc.column_name.clone(),
                                    scope_index: sc.index,
                                }),
                                data_type: sc.data_type,
                                nullable: sc.nullable,
                            },
                        });
                    }
                }
                SelectItem::Expr { expr, alias } => {
                    let bound = self.bind_expr(&expr, &scope)?;
                    let alias = alias.unwrap_or_else(|| infer_alias(&expr));
                    projections.push(BoundSelectItem { expr: bound, alias });
                }
            }
        }

        let output_columns = projections
            .iter()
            .map(|p| OutputColumn {
                name: p.alias.clone(),
                data_type: p.expr.data_type,
                nullable: p.expr.nullable,
            })
            .collect();

        let order_by = stmt
            .order_by
            .iter()
            .map(|o| {
                let expr = self.bind_order_by_expr(&o.expr, &scope, &projections)?;
                Ok(BoundOrderByExpr {
                    expr,
                    asc: o.asc,
                    nulls_first: o.nulls_first,
                })
            })
            .collect::<BindResult<Vec<_>>>()?;

        let limit = stmt
            .limit
            .as_ref()
            .map(|e| self.bind_expr(e, &scope))
            .transpose()?;
        let offset = stmt
            .offset
            .as_ref()
            .map(|e| self.bind_expr(e, &scope))
            .transpose()?;

        Ok(BoundSelect {
            projections,
            from: bound_from,
            joins: bound_joins,
            where_clause,
            group_by,
            having,
            order_by,
            limit,
            offset,
            distinct: stmt.distinct,
            output_columns,
        })
    }

    fn bind_table_ref(&self, table_ref: TableRef, nullable: bool) -> BindResult<BoundTableRef> {
        match table_ref {
            TableRef::Named { name, alias } => {
                let table = self
                    .accessor
                    .catalog_get_table_by_name(self.txn, &name)
                    .map_err(|_| BindError::UnknownTable(name))?;
                let mut columns = self
                    .accessor
                    .catalog_get_table_columns(self.txn, table.oid)
                    .map_err(|e| BindError::CatalogError(format!("{e:?}")))?;
                columns.sort_by_key(|c| c.position);
                Ok(BoundTableRef::BaseTable {
                    table,
                    columns,
                    alias,
                })
            }
            TableRef::Subquery { query, alias } => {
                let bound = self.bind_select(*query)?;
                Ok(BoundTableRef::Subquery {
                    query: Box::new(bound),
                    alias,
                })
            }
        }
    }

    fn add_table_ref_to_scope(&self, table_ref: &BoundTableRef, scope: &mut Scope, nullable: bool) {
        match table_ref {
            BoundTableRef::BaseTable {
                table,
                columns,
                alias,
            } => {
                let qualifier = alias.clone().unwrap_or_else(|| table.name.to_string());
                scope.add_table(Some(qualifier), columns, nullable);
            }
            BoundTableRef::Subquery { query, alias } => {
                for out_col in &query.output_columns {
                    scope.add_derived(
                        out_col.name.clone(),
                        out_col.data_type,
                        out_col.nullable || nullable,
                    );
                    let idx = scope.columns.len() - 1;
                    scope.columns[idx].qualifier = Some(alias.clone());
                }
            }
        }
    }

    fn bind_join_condition(
        &self,
        condition: JoinCondition,
        kind: JoinKind,
        scope: &Scope,
        right: &BoundTableRef,
    ) -> BindResult<BoundJoinCondition> {
        match condition {
            JoinCondition::On(expr) => Ok(BoundJoinCondition::On(self.bind_expr(&expr, scope)?)),
            JoinCondition::Using(col_names) => {
                let mut exprs = Vec::with_capacity(col_names.len());
                for col_name in &col_names {
                    let left_sc = scope.resolve(None, col_name)?;
                    let left_expr = BoundExpr {
                        kind: BoundExprKind::ColumnRef(BoundColumnRef {
                            qualifier: left_sc.qualifier.clone(),
                            column_name: left_sc.column_name.clone(),
                            scope_index: left_sc.index,
                        }),
                        data_type: left_sc.data_type,
                        nullable: left_sc.nullable,
                    };
                    let right_col_name = col_name;
                    let right_sc = scope
                        .columns
                        .iter()
                        .filter(|sc| sc.column_name.eq_ignore_ascii_case(right_col_name))
                        .last()
                        .ok_or_else(|| BindError::UnknownColumn(right_col_name.clone()))?;
                    let right_expr = BoundExpr {
                        kind: BoundExprKind::ColumnRef(BoundColumnRef {
                            qualifier: right_sc.qualifier.clone(),
                            column_name: right_sc.column_name.clone(),
                            scope_index: right_sc.index,
                        }),
                        data_type: right_sc.data_type,
                        nullable: right_sc.nullable,
                    };
                    exprs.push(BoundExpr {
                        kind: BoundExprKind::BinaryOp {
                            left: Box::new(left_expr),
                            op: BinaryOp::Eq,
                            right: Box::new(right_expr),
                        },
                        data_type: DataType::Bool,
                        nullable: false,
                    });
                }
                Ok(BoundJoinCondition::Using(exprs))
            }
            JoinCondition::Natural => {
                let right_cols: Vec<String> = match right {
                    BoundTableRef::BaseTable { columns, .. } => columns
                        .iter()
                        .map(|c| c.name.to_string().to_lowercase())
                        .collect(),
                    BoundTableRef::Subquery { query, .. } => query
                        .output_columns
                        .iter()
                        .map(|c| c.name.to_lowercase())
                        .collect(),
                };
                let mut exprs = Vec::new();
                for right_name in &right_cols {
                    if let Ok(sc) = scope.resolve(None, right_name) {
                        let left_expr = BoundExpr {
                            kind: BoundExprKind::ColumnRef(BoundColumnRef {
                                qualifier: sc.qualifier.clone(),
                                column_name: sc.column_name.clone(),
                                scope_index: sc.index,
                            }),
                            data_type: sc.data_type,
                            nullable: sc.nullable,
                        };
                        let right_sc = scope
                            .columns
                            .iter()
                            .filter(|c| c.column_name.eq_ignore_ascii_case(right_name))
                            .last()
                            .unwrap();
                        let right_expr = BoundExpr {
                            kind: BoundExprKind::ColumnRef(BoundColumnRef {
                                qualifier: right_sc.qualifier.clone(),
                                column_name: right_sc.column_name.clone(),
                                scope_index: right_sc.index,
                            }),
                            data_type: right_sc.data_type,
                            nullable: right_sc.nullable,
                        };
                        exprs.push(BoundExpr {
                            kind: BoundExprKind::BinaryOp {
                                left: Box::new(left_expr),
                                op: BinaryOp::Eq,
                                right: Box::new(right_expr),
                            },
                            data_type: DataType::Bool,
                            nullable: false,
                        });
                    }
                }
                Ok(BoundJoinCondition::Natural(exprs))
            }
            JoinCondition::None => Ok(BoundJoinCondition::None),
        }
    }

    fn bind_order_by_expr(
        &self,
        expr: &Expr,
        scope: &Scope,
        projections: &[BoundSelectItem],
    ) -> BindResult<BoundExpr> {
        if let Expr::Literal(Literal::Integer(n)) = expr {
            let idx = (*n as usize).wrapping_sub(1);
            if idx < projections.len() {
                return Ok(projections[idx].expr.clone());
            }
            return Err(BindError::Unsupported(format!(
                "ORDER BY position {n} out of range"
            )));
        }
        if let Expr::Column(col_ref) = expr {
            if col_ref.table.is_none() {
                if let Some(proj) = projections
                    .iter()
                    .find(|p| p.alias.eq_ignore_ascii_case(&col_ref.column))
                {
                    return Ok(proj.expr.clone());
                }
            }
        }
        self.bind_expr(expr, scope)
    }

    fn bind_expr(&self, expr: &Expr, scope: &Scope) -> BindResult<BoundExpr> {
        match expr {
            Expr::Literal(lit) => Ok(bind_literal(lit)),

            Expr::Column(col_ref) => {
                let sc = scope.resolve(col_ref.table.as_deref(), &col_ref.column)?;
                Ok(BoundExpr {
                    kind: BoundExprKind::ColumnRef(BoundColumnRef {
                        qualifier: sc.qualifier.clone(),
                        column_name: sc.column_name.clone(),
                        scope_index: sc.index,
                    }),
                    data_type: sc.data_type,
                    nullable: sc.nullable,
                })
            }

            Expr::BinaryOp { left, op, right } => {
                let mut l = self.bind_expr(left, scope)?;
                let mut r = self.bind_expr(right, scope)?;

                // Coerce numeric types to a common wider type so strict equality works
                if is_numeric(l.data_type) && is_numeric(r.data_type) {
                    if let Some(common) = wider_numeric(l.data_type, r.data_type) {
                        l = self.coerce_expr(l, common, "binary op left")?;
                        r = self.coerce_expr(r, common, "binary op right")?;
                    }
                }

                let (dt, nullable) = infer_binary_type(op, &l, &r)?;
                Ok(BoundExpr {
                    kind: BoundExprKind::BinaryOp {
                        left: Box::new(l),
                        op: op.clone(),
                        right: Box::new(r),
                    },
                    data_type: dt,
                    nullable,
                })
            }

            Expr::UnaryOp { op, expr } => {
                let inner = self.bind_expr(expr, scope)?;
                let dt = match op {
                    UnaryOp::Neg => {
                        if !is_numeric(inner.data_type) {
                            return Err(BindError::TypeMismatch {
                                expected: DataType::I64,
                                found: inner.data_type,
                                context: "unary minus".into(),
                            });
                        }
                        inner.data_type
                    }
                    UnaryOp::Not => DataType::Bool,
                };
                let nullable = inner.nullable;
                Ok(BoundExpr {
                    kind: BoundExprKind::UnaryOp {
                        op: op.clone(),
                        expr: Box::new(inner),
                    },
                    data_type: dt,
                    nullable,
                })
            }

            Expr::IsNull(inner) => {
                let bound = self.bind_expr(inner, scope)?;
                Ok(BoundExpr {
                    kind: BoundExprKind::IsNull(Box::new(bound)),
                    data_type: DataType::Bool,
                    nullable: false,
                })
            }

            Expr::IsNotNull(inner) => {
                let bound = self.bind_expr(inner, scope)?;
                Ok(BoundExpr {
                    kind: BoundExprKind::IsNotNull(Box::new(bound)),
                    data_type: DataType::Bool,
                    nullable: false,
                })
            }

            Expr::Like {
                expr,
                pattern,
                negated,
            } => {
                let bound_expr = self.bind_expr(expr, scope)?;
                let bound_pat = self.bind_expr(pattern, scope)?;
                Ok(BoundExpr {
                    kind: BoundExprKind::Like {
                        expr: Box::new(bound_expr),
                        pattern: Box::new(bound_pat),
                        negated: *negated,
                    },
                    data_type: DataType::Bool,
                    nullable: false,
                })
            }

            Expr::Between {
                expr,
                low,
                high,
                negated,
            } => {
                let bound_expr = self.bind_expr(expr, scope)?;
                let bound_low = self.bind_expr(low, scope)?;
                let bound_high = self.bind_expr(high, scope)?;
                Ok(BoundExpr {
                    kind: BoundExprKind::Between {
                        expr: Box::new(bound_expr),
                        low: Box::new(bound_low),
                        high: Box::new(bound_high),
                        negated: *negated,
                    },
                    data_type: DataType::Bool,
                    nullable: false,
                })
            }

            Expr::In {
                expr,
                list,
                negated,
            } => {
                let bound_expr = self.bind_expr(expr, scope)?;
                let bound_list = list
                    .iter()
                    .map(|e| self.bind_expr(e, scope))
                    .collect::<BindResult<Vec<_>>>()?;
                Ok(BoundExpr {
                    kind: BoundExprKind::In {
                        expr: Box::new(bound_expr),
                        list: bound_list,
                        negated: *negated,
                    },
                    data_type: DataType::Bool,
                    nullable: false,
                })
            }

            Expr::InSubquery {
                expr,
                subquery,
                negated,
            } => {
                let bound_expr = self.bind_expr(expr, scope)?;
                let bound_sub = self.bind_select(*subquery.clone())?;
                if bound_sub.output_columns.len() != 1 {
                    return Err(BindError::SubqueryTooManyColumns);
                }
                Ok(BoundExpr {
                    kind: BoundExprKind::InSubquery {
                        expr: Box::new(bound_expr),
                        subquery: Box::new(bound_sub),
                        negated: *negated,
                    },
                    data_type: DataType::Bool,
                    nullable: false,
                })
            }

            Expr::Exists { subquery, negated } => {
                let bound_sub = self.bind_select(*subquery.clone())?;
                Ok(BoundExpr {
                    kind: BoundExprKind::Exists {
                        subquery: Box::new(bound_sub),
                        negated: *negated,
                    },
                    data_type: DataType::Bool,
                    nullable: false,
                })
            }

            Expr::Subquery(subquery) => {
                let bound_sub = self.bind_select(*subquery.clone())?;
                if bound_sub.output_columns.len() != 1 {
                    return Err(BindError::SubqueryTooManyColumns);
                }
                let dt = bound_sub.output_columns[0].data_type;
                let nullable = bound_sub.output_columns[0].nullable;
                Ok(BoundExpr {
                    kind: BoundExprKind::Subquery(Box::new(bound_sub)),
                    data_type: dt,
                    nullable,
                })
            }

            Expr::Function {
                name,
                args,
                distinct,
            } => {
                let kind = FunctionKind::from(name.as_str());
                let bound_args = args
                    .iter()
                    .map(|a| {
                        if matches!(a, Expr::Wildcard) {
                            Ok(BoundExpr {
                                kind: BoundExprKind::Literal(BoundLiteral::Integer(1)),
                                data_type: DataType::I64,
                                nullable: false,
                            })
                        } else {
                            self.bind_expr(a, scope)
                        }
                    })
                    .collect::<BindResult<Vec<_>>>()?;

                let (dt, nullable) = infer_function_type(&kind, &bound_args);

                Ok(BoundExpr {
                    kind: BoundExprKind::Function(BoundFunction {
                        name: name.clone(),
                        kind,
                        args: bound_args,
                        distinct: *distinct,
                    }),
                    data_type: dt,
                    nullable,
                })
            }

            Expr::Cast { expr, data_type } => {
                let inner = self.bind_expr(expr, scope)?;
                Ok(BoundExpr {
                    nullable: inner.nullable,
                    kind: BoundExprKind::Cast {
                        expr: Box::new(inner),
                        target_type: *data_type,
                    },
                    data_type: *data_type,
                })
            }

            Expr::Case {
                operand,
                when_then,
                else_result,
            } => {
                let bound_operand = operand
                    .as_ref()
                    .map(|o| self.bind_expr(o, scope).map(Box::new))
                    .transpose()?;

                let bound_when_then = when_then
                    .iter()
                    .map(|(w, t)| Ok((self.bind_expr(w, scope)?, self.bind_expr(t, scope)?)))
                    .collect::<BindResult<Vec<_>>>()?;

                let bound_else = else_result
                    .as_ref()
                    .map(|e| self.bind_expr(e, scope).map(Box::new))
                    .transpose()?;

                let dt = bound_when_then
                    .first()
                    .map(|(_, t)| t.data_type)
                    .or_else(|| bound_else.as_ref().map(|e| e.data_type))
                    .unwrap_or(DataType::String);

                let nullable = bound_else.is_none()
                    || bound_when_then.iter().any(|(_, t)| t.nullable)
                    || bound_else.as_ref().map(|e| e.nullable).unwrap_or(false);

                Ok(BoundExpr {
                    kind: BoundExprKind::Case {
                        operand: bound_operand,
                        when_then: bound_when_then,
                        else_result: bound_else,
                    },
                    data_type: dt,
                    nullable,
                })
            }

            Expr::Wildcard => Err(BindError::Unsupported(
                "wildcard (*) is only valid in SELECT or COUNT(*)".into(),
            )),
        }
    }

    fn bind_default_expr(&self, expr: &Expr) -> BindResult<BoundExpr> {
        match expr {
            Expr::Column(_) => Err(BindError::InvalidDefault(
                "default value cannot reference a column".into(),
            )),
            Expr::Function { name, .. } => {
                let kind = FunctionKind::from(name.as_str());
                if kind.is_aggregate() {
                    return Err(BindError::InvalidDefault(
                        "default value cannot contain an aggregate function".into(),
                    ));
                }
                self.bind_expr(expr, &Scope::new())
            }
            other => self.bind_expr(other, &Scope::new()),
        }
    }

    fn coerce_expr(
        &self,
        expr: BoundExpr,
        target: DataType,
        context: &str,
    ) -> BindResult<BoundExpr> {
        if expr.data_type == target {
            return Ok(expr);
        }
        
        if matches!(expr.kind, BoundExprKind::Literal(BoundLiteral::Null)) {
            return Ok(BoundExpr {
                data_type: target,
                ..expr
            });
        }

        if let BoundExprKind::Literal(BoundLiteral::Integer(n)) = expr.kind {
            let fits = match target {
                DataType::I8  => i8::try_from(n).is_ok(),
                DataType::I16 => i16::try_from(n).is_ok(),
                DataType::I32 => i32::try_from(n).is_ok(),
                DataType::I64 => true,
                DataType::U8  => u8::try_from(n).is_ok(),
                DataType::U16 => u16::try_from(n).is_ok(),
                DataType::U32 => u32::try_from(n).is_ok(),
                DataType::U64 => n >= 0,
                DataType::F32 | DataType::F64 => true,
                _ => false,
            };

            if fits {
                return Ok(BoundExpr {
                    data_type: target,
                    ..expr
                });
            } else {
                return Err(BindError::TypeMismatch {
                    expected: target,
                    found: expr.data_type,
                    context: format!("literal {} is out of bounds for {} in {}", n, target, context),
                });
            }
        }

        if is_numeric(expr.data_type) && is_numeric(target) {
            if numeric_rank(target) >= numeric_rank(expr.data_type) {
                return Ok(BoundExpr {
                    nullable: expr.nullable,
                    kind: BoundExprKind::Cast {
                        expr: Box::new(expr),
                        target_type: target,
                    },
                    data_type: target,
                });
            }
        }

        Err(BindError::TypeMismatch {
            expected: target,
            found: expr.data_type,
            context: context.to_string(),
        })
    }

    fn expect_bool_or_numeric(&self, expr: &BoundExpr, context: &str) -> BindResult<()> {
        match expr.data_type {
            DataType::Bool => Ok(()),
            dt if is_numeric(dt) => Ok(()),
            dt => Err(BindError::TypeMismatch {
                expected: DataType::Bool,
                found: dt,
                context: context.to_string(),
            }),
        }
    }
}

fn bind_literal(lit: &Literal) -> BoundExpr {
    match lit {
        Literal::Integer(n) => BoundExpr {
            kind: BoundExprKind::Literal(BoundLiteral::Integer(*n)),
            data_type: DataType::I64,
            nullable: false,
        },
        Literal::Float(f) => BoundExpr {
            kind: BoundExprKind::Literal(BoundLiteral::Float(*f)),
            data_type: DataType::F64,
            nullable: false,
        },
        Literal::String(s) => BoundExpr {
            kind: BoundExprKind::Literal(BoundLiteral::String(s.clone())),
            data_type: DataType::String,
            nullable: false,
        },
        Literal::Bool(b) => BoundExpr {
            kind: BoundExprKind::Literal(BoundLiteral::Bool(*b)),
            data_type: DataType::Bool,
            nullable: false,
        },
        Literal::Null => BoundExpr {
            kind: BoundExprKind::Literal(BoundLiteral::Null),
            data_type: DataType::String,
            nullable: true,
        },
    }
}

fn infer_binary_type(
    op: &BinaryOp,
    left: &BoundExpr,
    right: &BoundExpr,
) -> BindResult<(DataType, bool)> {
    let nullable = left.nullable || right.nullable;
    match op {
        BinaryOp::Eq
        | BinaryOp::NotEq
        | BinaryOp::Lt
        | BinaryOp::LtEq
        | BinaryOp::Gt
        | BinaryOp::GtEq => Ok((DataType::Bool, nullable)),

        BinaryOp::And | BinaryOp::Or => Ok((DataType::Bool, nullable)),

        BinaryOp::Concat => Ok((DataType::String, nullable)),

        BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => {
            let dt = wider_numeric(left.data_type, right.data_type).ok_or_else(|| {
                BindError::TypeMismatch {
                    expected: left.data_type,
                    found: right.data_type,
                    context: format!("{op:?}"),
                }
            })?;
            Ok((dt, nullable))
        }
    }
}

fn infer_function_type(kind: &FunctionKind, args: &[BoundExpr]) -> (DataType, bool) {
    match kind {
        FunctionKind::Count => (DataType::I64, false),
        FunctionKind::Sum | FunctionKind::Avg => {
            let dt = args.first().map(|a| a.data_type).unwrap_or(DataType::F64);
            (
                if is_integer(dt) {
                    DataType::I64
                } else {
                    DataType::F64
                },
                true,
            )
        }
        FunctionKind::Min | FunctionKind::Max => {
            let dt = args.first().map(|a| a.data_type).unwrap_or(DataType::I64);
            (dt, true)
        }
        FunctionKind::Upper | FunctionKind::Lower => (DataType::String, true),
        FunctionKind::Length => (DataType::I64, true),
        FunctionKind::Abs => {
            let dt = args.first().map(|a| a.data_type).unwrap_or(DataType::I64);
            (dt, true)
        }
        FunctionKind::Coalesce => {
            let dt = args
                .first()
                .map(|a| a.data_type)
                .unwrap_or(DataType::String);
            let nullable = args.iter().all(|a| a.nullable);
            (dt, nullable)
        }
        FunctionKind::Nullif => {
            let dt = args
                .first()
                .map(|a| a.data_type)
                .unwrap_or(DataType::String);
            (dt, true)
        }
        FunctionKind::Unknown => (DataType::String, true),
    }
}

fn translate_join_kind(kind: JoinKind) -> BoundJoinKind {
    match kind {
        JoinKind::Inner => BoundJoinKind::Inner,
        JoinKind::LeftOuter => BoundJoinKind::LeftOuter,
        JoinKind::RightOuter => BoundJoinKind::RightOuter,
        JoinKind::FullOuter => BoundJoinKind::FullOuter,
        JoinKind::Cross => BoundJoinKind::Cross,
    }
}

fn is_numeric(dt: DataType) -> bool {
    matches!(
        dt,
        DataType::I8
            | DataType::I16
            | DataType::I32
            | DataType::I64
            | DataType::U8
            | DataType::U16
            | DataType::U32
            | DataType::F32
            | DataType::F64
    )
}

fn is_integer(dt: DataType) -> bool {
    matches!(
        dt,
        DataType::I8
            | DataType::I16
            | DataType::I32
            | DataType::I64
            | DataType::U8
            | DataType::U16
            | DataType::U32
    )
}

fn numeric_rank(dt: DataType) -> u8 {
    match dt {
        DataType::I8 | DataType::U8 => 1,
        DataType::I16 | DataType::U16 => 2,
        DataType::I32 | DataType::U32 => 3,
        DataType::I64 => 4,
        DataType::F32 => 5,
        DataType::F64 => 6,
        _ => 0,
    }
}

fn wider_numeric(a: DataType, b: DataType) -> Option<DataType> {
    if !is_numeric(a) || !is_numeric(b) {
        return None;
    }
    if numeric_rank(a) >= numeric_rank(b) {
        Some(a)
    } else {
        Some(b)
    }
}

fn infer_alias(expr: &Expr) -> String {
    match expr {
        Expr::Column(col_ref) => col_ref.column.clone(),
        Expr::Function { name, .. } => name.clone(),
        _ => "?column?".to_string(),
    }
}
