use crate::{
    binder::{
        BoundAlterTable, BoundAssignment, BoundCreateIndex, BoundCreateTable, BoundDropIndex,
        BoundDropTable, BoundExpr, BoundJoinCondition, BoundJoinKind, FunctionKind, OutputColumn,
    },
    catalog,
    databox::DataType,
    planner::Schema,
};

#[derive(Debug, Clone)]
pub enum PhysicalPlan {
    SeqScan(PhysSeqScan),
    IndexScan(PhysIndexScan),
    Filter(PhysFilter),
    Projection(PhysProjection),
    NestedLoopJoin(PhysNestedLoopJoin),
    HashJoin(PhysHashJoin),
    HashAggregate(PhysHashAggregate),
    StreamAggregate(PhysStreamAggregate),
    Sort(PhysSort),
    TopN(PhysTopN),
    Limit(PhysLimit),
    Distinct(PhysDistinct),
    HashDistinct(PhysHashDistinct),
    Insert(PhysInsert),
    Update(PhysUpdate),
    Delete(PhysDelete),
    Values(PhysValues),
    CreateTable(BoundCreateTable),
    DropTable(BoundDropTable),
    AlterTable(BoundAlterTable),
    CreateIndex(BoundCreateIndex),
    DropIndex(BoundDropIndex),
    DescribeTable(catalog::Table, Vec<catalog::Column>),
    ShowTables,
    Nothing,
}

impl PhysicalPlan {
    pub fn schema(&self) -> Schema {
        match self {
            PhysicalPlan::SeqScan(n) => n.schema.clone(),
            PhysicalPlan::IndexScan(n) => n.schema.clone(),
            PhysicalPlan::Filter(n) => n.input.schema(),
            PhysicalPlan::Projection(n) => n.schema.clone(),
            PhysicalPlan::NestedLoopJoin(n) => n.schema.clone(),
            PhysicalPlan::HashJoin(n) => n.schema.clone(),
            PhysicalPlan::HashAggregate(n) => n.schema.clone(),
            PhysicalPlan::StreamAggregate(n) => n.schema.clone(),
            PhysicalPlan::Sort(n) => n.input.schema(),
            PhysicalPlan::TopN(n) => n.input.schema(),
            PhysicalPlan::Limit(n) => n.input.schema(),
            PhysicalPlan::Distinct(n) => n.input.schema(),
            PhysicalPlan::HashDistinct(n) => n.input.schema(),
            PhysicalPlan::Insert(n) => n.schema.clone(),
            PhysicalPlan::Update(n) => n.schema.clone(),
            PhysicalPlan::Delete(n) => n.schema.clone(),
            PhysicalPlan::Values(n) => n.schema.clone(),
            PhysicalPlan::CreateTable(_)
            | PhysicalPlan::DropTable(_)
            | PhysicalPlan::AlterTable(_)
            | PhysicalPlan::CreateIndex(_)
            | PhysicalPlan::DropIndex(_)
            | PhysicalPlan::DescribeTable(_, _)
            | PhysicalPlan::ShowTables
            | PhysicalPlan::Nothing => Schema::empty(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PhysSeqScan {
    pub table: catalog::Table,
    pub columns: Vec<catalog::Column>,
    pub alias: Option<String>,
    pub pushed_predicates: Vec<BoundExpr>,
    pub schema: Schema,
}

#[derive(Debug, Clone)]
pub struct PhysIndexScan {
    pub index: catalog::Index,
    pub table: catalog::Table,
    pub columns: Vec<catalog::Column>,
    pub start_key: Option<BoundExpr>,
    pub end_key: Option<BoundExpr>,
    pub residual_predicates: Vec<BoundExpr>,
    pub schema: Schema,
}

#[derive(Debug, Clone)]
pub struct PhysFilter {
    pub predicate: BoundExpr,
    pub input: Box<PhysicalPlan>,
}

#[derive(Debug, Clone)]
pub struct PhysProjection {
    pub exprs: Vec<BoundExpr>,
    pub aliases: Vec<String>,
    pub input: Box<PhysicalPlan>,
    pub schema: Schema,
}

#[derive(Debug, Clone)]
pub struct PhysNestedLoopJoin {
    pub kind: BoundJoinKind,
    pub left: Box<PhysicalPlan>,
    pub right: Box<PhysicalPlan>,
    pub condition: BoundJoinCondition,
    pub schema: Schema,
}

#[derive(Debug, Clone)]
pub struct PhysHashJoin {
    pub kind: BoundJoinKind,
    pub left: Box<PhysicalPlan>,
    pub right: Box<PhysicalPlan>,
    pub left_keys: Vec<BoundExpr>,
    pub right_keys: Vec<BoundExpr>,
    pub residual: Option<BoundExpr>,
    pub schema: Schema,
}

#[derive(Debug, Clone)]
pub struct PhysHashAggregate {
    pub group_by: Vec<BoundExpr>,
    pub aggregates: Vec<PhysAggregateExpr>,
    pub input: Box<PhysicalPlan>,
    pub schema: Schema,
}

#[derive(Debug, Clone)]
pub struct PhysStreamAggregate {
    pub group_by: Vec<BoundExpr>,
    pub aggregates: Vec<PhysAggregateExpr>,
    pub input: Box<PhysicalPlan>,
    pub schema: Schema,
}

#[derive(Debug, Clone)]
pub struct PhysAggregateExpr {
    pub kind: FunctionKind,
    pub arg: Option<BoundExpr>,
    pub distinct: bool,
    pub alias: String,
    pub data_type: DataType,
    pub nullable: bool,
}

#[derive(Debug, Clone)]
pub struct PhysSort {
    pub order_by: Vec<PhysSortKey>,
    pub input: Box<PhysicalPlan>,
}

#[derive(Debug, Clone)]
pub struct PhysTopN {
    pub order_by: Vec<PhysSortKey>,
    pub limit: BoundExpr,
    pub offset: Option<BoundExpr>,
    pub input: Box<PhysicalPlan>,
}

#[derive(Debug, Clone)]
pub struct PhysSortKey {
    pub expr: BoundExpr,
    pub asc: bool,
    pub nulls_first: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct PhysLimit {
    pub limit: Option<BoundExpr>,
    pub offset: Option<BoundExpr>,
    pub input: Box<PhysicalPlan>,
}

#[derive(Debug, Clone)]
pub struct PhysDistinct {
    pub input: Box<PhysicalPlan>,
}

#[derive(Debug, Clone)]
pub struct PhysHashDistinct {
    pub input: Box<PhysicalPlan>,
}

#[derive(Debug, Clone)]
pub struct PhysInsert {
    pub table: catalog::Table,
    pub table_columns: Vec<catalog::Column>,
    pub target_columns: Vec<catalog::Column>,
    pub source: Box<PhysicalPlan>,
    pub schema: Schema,
}

#[derive(Debug, Clone)]
pub struct PhysUpdate {
    pub table: catalog::Table,
    pub table_columns: Vec<catalog::Column>,
    pub assignments: Vec<BoundAssignment>,
    pub input: Box<PhysicalPlan>,
    pub schema: Schema,
}

#[derive(Debug, Clone)]
pub struct PhysDelete {
    pub table: catalog::Table,
    pub table_columns: Vec<catalog::Column>,
    pub input: Box<PhysicalPlan>,
    pub schema: Schema,
}

#[derive(Debug, Clone)]
pub struct PhysValues {
    pub rows: Vec<Vec<BoundExpr>>,
    pub schema: Schema,
}
