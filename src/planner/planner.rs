use super::plan_nodes::*;
use crate::binder::{BoundExpr, BoundSelect, BoundStatement, BoundTableRef};
use crate::catalog::Catalog;

#[derive(Debug, Clone, Copy)]
pub enum PlanError {
    UnsupportedStatement,
    InvalidJoinCondition,
    InvalidProjection,
    UnknownTable,
    InvalidFilter,
    SchemaError,
}

pub struct Planner<'a> {
    catalog: &'a Catalog,
}

impl<'a> Planner<'a> {
    pub fn new(catalog: &'a Catalog) -> Self {
        Self { catalog }
    }

    pub fn plan(&self, stmt: &BoundStatement) -> Result<PlanNode, PlanError> {
        todo!()
    }

    fn plan_create_table(
        &self,
        stmt: &crate::binder::BoundCreateTable,
    ) -> Result<PlanNode, PlanError> {
        todo!()
    }

    fn plan_insert(&self, stmt: &crate::binder::BoundInsert) -> Result<PlanNode, PlanError> {
        todo!()
    }

    fn plan_update(&self, stmt: &crate::binder::BoundUpdate) -> Result<PlanNode, PlanError> {
        todo!()
    }

    fn plan_delete(&self, stmt: &crate::binder::BoundDelete) -> Result<PlanNode, PlanError> {
        todo!()
    }

    fn plan_select(&self, stmt: &BoundSelect) -> Result<PlanNode, PlanError> {
        todo!()
    }

    fn plan_table_ref(&self, table_ref: &BoundTableRef) -> Result<PlanNode, PlanError> {
        todo!()
    }

    fn plan_base_table(&self, table_name: &str, table_oid: u32) -> Result<PlanNode, PlanError> {
        todo!()
    }

    fn plan_join(
        &self,
        join_type: crate::binder::JoinType,
        left: &BoundTableRef,
        right: &BoundTableRef,
        condition: Option<&BoundExpr>,
    ) -> Result<PlanNode, PlanError> {
        todo!()
    }
}
