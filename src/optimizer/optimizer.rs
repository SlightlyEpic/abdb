use crate::catalog::Catalog;
use crate::planner::plan_nodes::*;

pub struct Optimizer<'a> {
    catalog: &'a Catalog,
}

impl<'a> Optimizer<'a> {
    pub fn new(catalog: &'a Catalog) -> Self {
        Self { catalog }
    }

    pub fn optimize(&self, plan: PlanNode) -> PlanNode {
        let plan = self.push_down_filters(plan);
        let plan = self.push_down_projections(plan);
        let plan = self.reorder_joins(plan);
        let plan = self.choose_join_algorithm(plan);
        let plan = self.choose_access_method(plan);
        let plan = self.merge_operators(plan);
        plan
    }

    fn push_down_filters(&self, mut node: PlanNode) -> PlanNode {
        todo!()
    }

    fn push_down_projections(&self, mut node: PlanNode) -> PlanNode {
        todo!()
    }

    fn reorder_joins(&self, mut node: PlanNode) -> PlanNode {
        todo!()
    }

    fn choose_join_algorithm(&self, mut node: PlanNode) -> PlanNode {
        todo!()
    }

    fn choose_access_method(&self, mut node: PlanNode) -> PlanNode {
        todo!()
    }

    fn merge_operators(&self, mut node: PlanNode) -> PlanNode {
        todo!()
    }
}
