use crate::binder::{BoundExpr, ColumnDef, DataType, JoinType};

#[derive(Debug, Clone)]
pub struct Schema {
    pub columns: Vec<(String, DataType)>,
}

impl Schema {
    pub fn new(columns: Vec<(String, DataType)>) -> Self {
        Self { columns }
    }

    pub fn empty() -> Self {
        Self {
            columns: Vec::new(),
        }
    }

    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    pub fn get_column(&self, idx: usize) -> Option<&(String, DataType)> {
        self.columns.get(idx)
    }
}

#[derive(Debug)]
pub enum PlanNode {
    CreateTable(CreateTableNode),
    DropTable(DropTableNode),

    Insert(InsertNode),
    Update(UpdateNode),
    Delete(DeleteNode),

    SeqScan(SeqScanNode),
    IndexScan(IndexScanNode),
    Filter(FilterNode),
    Projection(ProjectionNode),

    NestedLoopJoin(NestedLoopJoinNode),
    HashJoin(HashJoinNode),
    MergeJoin(MergeJoinNode),

    Sort(SortNode),
    Limit(LimitNode),

    Values(ValuesNode),
}

impl PlanNode {
    pub fn schema(&self) -> &Schema {
        match self {
            PlanNode::CreateTable(n) => &n.schema,
            PlanNode::DropTable(n) => &n.schema,
            PlanNode::Insert(n) => &n.schema,
            PlanNode::Update(n) => &n.schema,
            PlanNode::Delete(n) => &n.schema,
            PlanNode::SeqScan(n) => &n.schema,
            PlanNode::IndexScan(n) => &n.schema,
            PlanNode::Filter(n) => &n.schema,
            PlanNode::Projection(n) => &n.schema,
            PlanNode::NestedLoopJoin(n) => &n.schema,
            PlanNode::HashJoin(n) => &n.schema,
            PlanNode::MergeJoin(n) => &n.schema,
            PlanNode::Sort(n) => &n.schema,
            PlanNode::Limit(n) => &n.schema,
            PlanNode::Values(n) => &n.schema,
        }
    }
}

// DDL Nodes
#[derive(Debug)]
pub struct CreateTableNode {
    pub schema: Schema,
    pub table_name: String,
    pub columns: Vec<ColumnDef>,
    pub primary_key: Vec<String>,
}

#[derive(Debug)]
pub struct DropTableNode {
    pub schema: Schema,
    pub table_name: String,
    pub table_oid: u32,
}

#[derive(Debug)]
pub struct InsertNode {
    pub schema: Schema,
    pub table_name: String,
    pub table_oid: u32,
    pub columns: Vec<usize>,
    pub child: Box<PlanNode>,
}

#[derive(Debug)]
pub struct UpdateNode {
    pub schema: Schema,
    pub table_name: String,
    pub table_oid: u32,
    pub assignments: Vec<(usize, BoundExpr)>,
    pub child: Box<PlanNode>,
}

#[derive(Debug)]
pub struct DeleteNode {
    pub schema: Schema,
    pub table_name: String,
    pub table_oid: u32,
    pub child: Box<PlanNode>,
}

#[derive(Debug)]
pub struct SeqScanNode {
    pub schema: Schema,
    pub table_name: String,
    pub table_oid: u32,
    pub predicate: Option<BoundExpr>,
}

#[derive(Debug)]
pub struct IndexScanNode {
    pub schema: Schema,
    pub table_name: String,
    pub table_oid: u32,
    pub index_oid: u32,
    pub predicate: Option<BoundExpr>,
}

#[derive(Debug)]
pub struct FilterNode {
    pub schema: Schema,
    pub predicate: BoundExpr,
    pub child: Box<PlanNode>,
}

#[derive(Debug)]
pub struct ProjectionNode {
    pub schema: Schema,
    pub expressions: Vec<BoundExpr>,
    pub child: Box<PlanNode>,
}

// Join Nodes
#[derive(Debug)]
pub struct NestedLoopJoinNode {
    pub schema: Schema,
    pub join_type: JoinType,
    pub condition: Option<BoundExpr>,
    pub left: Box<PlanNode>,
    pub right: Box<PlanNode>,
}

#[derive(Debug)]
pub struct HashJoinNode {
    pub schema: Schema,
    pub join_type: JoinType,
    pub left_keys: Vec<BoundExpr>,
    pub right_keys: Vec<BoundExpr>,
    pub condition: Option<BoundExpr>,
    pub left: Box<PlanNode>,
    pub right: Box<PlanNode>,
}

#[derive(Debug)]
pub struct MergeJoinNode {
    pub schema: Schema,
    pub join_type: JoinType,
    pub left_keys: Vec<BoundExpr>,
    pub right_keys: Vec<BoundExpr>,
    pub left: Box<PlanNode>,
    pub right: Box<PlanNode>,
}

#[derive(Debug)]
pub struct SortNode {
    pub schema: Schema,
    pub order_by: Vec<(BoundExpr, bool)>,
    pub child: Box<PlanNode>,
}

#[derive(Debug)]
pub struct LimitNode {
    pub schema: Schema,
    pub limit: usize,
    pub offset: usize,
    pub child: Box<PlanNode>,
}

#[derive(Debug)]
pub struct ValuesNode {
    pub schema: Schema,
    pub values: Vec<Vec<BoundExpr>>,
}
