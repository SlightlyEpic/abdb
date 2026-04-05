use crate::{
    binder::{
        BoundAlterTable, BoundAssignment, BoundColumnDef, BoundCreateIndex, BoundCreateTable,
        BoundDropIndex, BoundDropTable, BoundExpr, BoundJoinCondition, BoundJoinKind, FunctionKind,
        OutputColumn,
    },
    catalog,
    common::aliases,
    databox::DataType,
};

#[derive(Debug, Clone)]
pub struct Schema {
    pub columns: Vec<OutputColumn>,
}

impl Schema {
    pub fn new(columns: Vec<OutputColumn>) -> Self {
        Self { columns }
    }

    pub fn empty() -> Self {
        Self { columns: vec![] }
    }

    pub fn len(&self) -> usize {
        self.columns.len()
    }
}

#[derive(Debug, Clone)]
pub enum LogicalPlan {
    // --- Scans ---
    SeqScan(SeqScan),
    IndexScan(IndexScan),

    // --- Filtering / projection ---
    Filter(Filter),
    Projection(Projection),

    // --- Joins ---
    Join(Join),

    // --- Aggregation ---
    Aggregate(Aggregate),

    // --- Sorting / limiting ---
    Sort(Sort),
    Limit(Limit),

    // --- Set ---
    Distinct(Distinct),

    // --- DML ---
    Insert(LogicalInsert),
    Update(LogicalUpdate),
    Delete(LogicalDelete),

    // --- DDL (leaf nodes, no children) ---
    CreateTable(BoundCreateTable),
    DropTable(BoundDropTable),
    AlterTable(BoundAlterTable),
    CreateIndex(BoundCreateIndex),
    DropIndex(BoundDropIndex),

    // --- Values (inline rows, e.g. INSERT ... VALUES) ---
    Values(Values),

    // --- No-op for transaction control ---
    Nothing,
}

impl LogicalPlan {
    pub fn schema(&self) -> Schema {
        match self {
            LogicalPlan::SeqScan(n) => n.schema.clone(),
            LogicalPlan::IndexScan(n) => n.schema.clone(),
            LogicalPlan::Filter(n) => n.input.schema(),
            LogicalPlan::Projection(n) => n.schema.clone(),
            LogicalPlan::Join(n) => n.schema.clone(),
            LogicalPlan::Aggregate(n) => n.schema.clone(),
            LogicalPlan::Sort(n) => n.input.schema(),
            LogicalPlan::Limit(n) => n.input.schema(),
            LogicalPlan::Distinct(n) => n.input.schema(),
            LogicalPlan::Insert(n) => n.schema.clone(),
            LogicalPlan::Update(n) => n.schema.clone(),
            LogicalPlan::Delete(n) => n.schema.clone(),
            LogicalPlan::Values(n) => n.schema.clone(),
            LogicalPlan::CreateTable(_)
            | LogicalPlan::DropTable(_)
            | LogicalPlan::AlterTable(_)
            | LogicalPlan::CreateIndex(_)
            | LogicalPlan::DropIndex(_)
            | LogicalPlan::Nothing => Schema::empty(),
        }
    }
}

// --- Scan nodes ---

#[derive(Debug, Clone)]
pub struct SeqScan {
    pub table: catalog::Table,
    pub columns: Vec<catalog::Column>,
    pub alias: Option<String>,
    pub schema: Schema,
}

#[derive(Debug, Clone)]
pub struct IndexScan {
    pub index: catalog::Index,
    pub table: catalog::Table,
    pub columns: Vec<catalog::Column>,
    pub start_key: Option<BoundExpr>,
    pub end_key: Option<BoundExpr>,
    pub schema: Schema,
}

// --- Unary nodes ---

#[derive(Debug, Clone)]
pub struct Filter {
    pub predicate: BoundExpr,
    pub input: Box<LogicalPlan>,
}

#[derive(Debug, Clone)]
pub struct Projection {
    pub exprs: Vec<BoundExpr>,
    pub aliases: Vec<String>,
    pub input: Box<LogicalPlan>,
    pub schema: Schema,
}

#[derive(Debug, Clone)]
pub struct Sort {
    pub order_by: Vec<SortKey>,
    pub input: Box<LogicalPlan>,
}

#[derive(Debug, Clone)]
pub struct SortKey {
    pub expr: BoundExpr,
    pub asc: bool,
    pub nulls_first: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct Limit {
    pub limit: Option<BoundExpr>,
    pub offset: Option<BoundExpr>,
    pub input: Box<LogicalPlan>,
}

#[derive(Debug, Clone)]
pub struct Distinct {
    pub input: Box<LogicalPlan>,
}

// --- Join ---

#[derive(Debug, Clone)]
pub struct Join {
    pub kind: BoundJoinKind,
    pub left: Box<LogicalPlan>,
    pub right: Box<LogicalPlan>,
    pub condition: BoundJoinCondition,
    pub schema: Schema,
}

// --- Aggregate ---

#[derive(Debug, Clone)]
pub struct Aggregate {
    pub group_by: Vec<BoundExpr>,
    pub aggregates: Vec<AggregateExpr>,
    pub input: Box<LogicalPlan>,
    pub schema: Schema,
}

#[derive(Debug, Clone)]
pub struct AggregateExpr {
    pub kind: FunctionKind,
    pub arg: Option<BoundExpr>,
    pub distinct: bool,
    pub alias: String,
    pub data_type: DataType,
    pub nullable: bool,
}

// --- Values ---

#[derive(Debug, Clone)]
pub struct Values {
    pub rows: Vec<Vec<BoundExpr>>,
    pub schema: Schema,
}

// --- DML ---

#[derive(Debug, Clone)]
pub struct LogicalInsert {
    pub table: catalog::Table,
    pub table_columns: Vec<catalog::Column>,
    pub target_columns: Vec<catalog::Column>,
    pub source: Box<LogicalPlan>,
    pub schema: Schema,
}

#[derive(Debug, Clone)]
pub struct LogicalUpdate {
    pub table: catalog::Table,
    pub table_columns: Vec<catalog::Column>,
    pub assignments: Vec<BoundAssignment>,
    pub input: Box<LogicalPlan>,
    pub schema: Schema,
}

#[derive(Debug, Clone)]
pub struct LogicalDelete {
    pub table: catalog::Table,
    pub table_columns: Vec<catalog::Column>,
    pub input: Box<LogicalPlan>,
    pub schema: Schema,
}
