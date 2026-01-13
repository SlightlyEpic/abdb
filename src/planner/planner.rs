use super::plan_nodes::*;
use crate::{
    accessor::Accessor,
    binder::{BoundExpr, BoundSelect, BoundStatement, BoundTableRef},
};

#[derive(Debug, Clone, Copy)]
pub enum PlanError {
    UnsupportedStatement,
    InvalidJoinCondition,
    InvalidProjection,
    UnknownTable,
    InvalidFilter,
    SchemaError,
}

pub struct Planner<'a, A: Accessor> {
    accessor: &'a A,
}

impl<'a, A: Accessor> Planner<'a, A> {
    pub fn new(accessor: &'a A) -> Self {
        Self { accessor }
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
