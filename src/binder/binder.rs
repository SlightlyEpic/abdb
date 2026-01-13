use crate::{accessor::Accessor, databox::DataType};

use super::{BoundExpr, BoundSelect, BoundStatement, BoundTableRef};
use sqlparser::ast;

#[derive(Debug, Clone)]
pub enum BindError {
    TableNotFound(String),
    ColumnNotFound(String),
    DuplicateColumn(String),
    TypeMismatch { expected: DataType, found: DataType },
    InvalidExpression(String),
    UnsupportedFeature(String),
    // todo
}

pub struct Binder<'a, A: Accessor> {
    accessor: &'a A,
    table_scope: Vec<TableScope>,
}

struct TableScope {
    table_name: String,
    table_oid: u32,
    alias: Option<String>,
    columns: Vec<(String, DataType)>,
}

impl<'a, A: Accessor> Binder<'a, A> {
    pub fn new(catalog: &'a A) -> Self {
        todo!()
    }

    pub fn bind_statement(&mut self, stmt: &ast::Statement) -> Result<BoundStatement, BindError> {
        todo!()
    }

    fn bind_create_table(
        &mut self,
        name: &ast::ObjectName,
        columns: &[ast::ColumnDef],
    ) -> Result<BoundStatement, BindError> {
        todo!()
    }

    fn bind_insert(
        &mut self,
        table_name: &ast::ObjectName,
        columns: &Option<Vec<ast::Ident>>,
        source: &ast::Query,
    ) -> Result<BoundStatement, BindError> {
        todo!()
    }

    fn bind_update(
        &mut self,
        table: &ast::TableWithJoins,
        assignments: &[ast::Assignment],
        where_clause: Option<&ast::Expr>,
    ) -> Result<BoundStatement, BindError> {
        todo!()
    }

    fn bind_delete(
        &mut self,
        from: &[ast::TableWithJoins],
        where_clause: Option<&ast::Expr>,
    ) -> Result<BoundStatement, BindError> {
        todo!()
    }

    fn bind_query(&mut self, query: &ast::Query) -> Result<BoundSelect, BindError> {
        todo!()
    }

    fn bind_select(&mut self, select: &ast::Select) -> Result<BoundSelect, BindError> {
        todo!()
    }

    fn bind_table_with_joins(
        &mut self,
        table: &ast::TableWithJoins,
    ) -> Result<BoundTableRef, BindError> {
        todo!()
    }

    fn bind_table_ref(&mut self, table: &ast::TableFactor) -> Result<BoundTableRef, BindError> {
        todo!()
    }

    fn bind_join_constraint(
        &mut self,
        constraint: &ast::JoinConstraint,
    ) -> Result<Option<BoundExpr>, BindError> {
        todo!()
    }

    fn bind_select_list(
        &mut self,
        projection: &[ast::SelectItem],
    ) -> Result<Vec<BoundExpr>, BindError> {
        todo!()
    }

    fn bind_expr(&mut self, expr: &ast::Expr) -> Result<BoundExpr, BindError> {
        todo!()
    }

    fn bind_value(&self, value: &ast::Value) -> Result<BoundExpr, BindError> {
        todo!()
    }

    fn bind_data_type(&self, data_type: &ast::DataType) -> Result<DataType, BindError> {
        todo!()
    }

    fn bind_binary_op(&self, op: &ast::BinaryOperator) -> Result<super::BinaryOperator, BindError> {
        todo!()
    }

    fn bind_unary_op(&self, op: &ast::UnaryOperator) -> Result<super::UnaryOperator, BindError> {
        todo!()
    }

    fn push_table_scope(&mut self, table_ref: &BoundTableRef) -> Result<(), BindError> {
        todo!()
    }

    fn pop_table_scope(&mut self) {
        todo!()
    }

    fn resolve_column(&self, col_name: &str, table_name: Option<&str>) -> Result<usize, BindError> {
        todo!()
    }

    fn get_expr_type(&self, expr: &BoundExpr) -> &DataType {
        todo!()
    }
}
