use crate::{
    catalog,
    common::aliases,
    databox::DataType,
    parser::ast::{BinaryOp, ForeignKeyClause, UnaryOp},
};

#[derive(Debug, Clone)]
pub enum BoundStatement {
    BeginTransaction,
    Commit,
    Rollback,
    CreateTable(BoundCreateTable),
    DropTable(BoundDropTable),
    AlterTable(BoundAlterTable),
    Insert(BoundInsert),
    Select(BoundSelect),
    Update(BoundUpdate),
    Delete(BoundDelete),
    CreateIndex(BoundCreateIndex),
    DropIndex(BoundDropIndex),
    Explain(Box<BoundStatement>),
}

#[derive(Debug, Clone)]
pub struct BoundCreateTable {
    pub table_oid: aliases::OId,
    pub file_id: aliases::FileId,
    pub name: String,
    pub columns: Vec<BoundColumnDef>,
    pub primary_key: Vec<u16>,
    pub foreign_keys: Vec<ForeignKeyClause>,
    pub if_not_exists: bool,
}

#[derive(Debug, Clone)]
pub struct BoundColumnDef {
    pub oid: aliases::OId,
    pub name: String,
    pub data_type: DataType,
    pub position: u16,
    pub nullable: bool,
    pub default: Option<BoundExpr>,
    pub unique: bool,
    pub primary_key: bool,
}

#[derive(Debug, Clone)]
pub struct BoundDropTable {
    pub table_oid: aliases::OId,
    pub name: String,
    pub index_oids: Vec<aliases::OId>,
    pub if_exists: bool,
}

#[derive(Debug, Clone)]
pub struct BoundAlterTable {
    pub table_oid: aliases::OId,
    pub name: String,
    pub action: BoundAlterAction,
}

#[derive(Debug, Clone)]
pub enum BoundAlterAction {
    AddColumn(BoundColumnDef),
    DropColumn {
        column_oid: aliases::OId,
        position: u16,
    },
    RenameColumn {
        column_oid: aliases::OId,
        old_name: String,
        new_name: String,
    },
    RenameTable {
        new_name: String,
    },
    AlterColumnType {
        column_oid: aliases::OId,
        new_type: DataType,
    },
    AddForeignKey(ForeignKeyClause),
    DropForeignKey(String),
    AddPrimaryKey(Vec<aliases::OId>),
    DropConstraint(String),
}

#[derive(Debug, Clone)]
pub struct BoundCreateIndex {
    pub index_oid: aliases::OId,
    pub file_id: aliases::FileId,
    pub name: String,
    pub table_oid: aliases::OId,
    pub column_oid: aliases::OId,
    pub unique: bool,
    pub if_not_exists: bool,
}

#[derive(Debug, Clone)]
pub struct BoundDropIndex {
    pub index_oid: aliases::OId,
    pub name: String,
    pub table_oid: aliases::OId,
    pub if_exists: bool,
}

#[derive(Debug, Clone)]
pub struct BoundInsert {
    pub table: catalog::Table,
    pub table_columns: Vec<catalog::Column>,
    pub target_columns: Vec<catalog::Column>,
    pub source: BoundInsertSource,
}

#[derive(Debug, Clone)]
pub enum BoundInsertSource {
    Values(Vec<Vec<BoundExpr>>),
    Select(Box<BoundSelect>),
}

#[derive(Debug, Clone)]
pub struct BoundUpdate {
    pub table: catalog::Table,
    pub table_columns: Vec<catalog::Column>,
    pub assignments: Vec<BoundAssignment>,
    pub where_clause: Option<BoundExpr>,
}

#[derive(Debug, Clone)]
pub struct BoundAssignment {
    pub column: catalog::Column,
    pub value: BoundExpr,
}

#[derive(Debug, Clone)]
pub struct BoundDelete {
    pub table: catalog::Table,
    pub table_columns: Vec<catalog::Column>,
    pub where_clause: Option<BoundExpr>,
}

#[derive(Debug, Clone)]
pub struct BoundSelect {
    pub projections: Vec<BoundSelectItem>,
    pub from: Vec<BoundTableRef>,
    pub joins: Vec<BoundJoin>,
    pub where_clause: Option<BoundExpr>,
    pub group_by: Vec<BoundExpr>,
    pub having: Option<BoundExpr>,
    pub order_by: Vec<BoundOrderByExpr>,
    pub limit: Option<BoundExpr>,
    pub offset: Option<BoundExpr>,
    pub distinct: bool,
    pub output_columns: Vec<OutputColumn>,
}

#[derive(Debug, Clone)]
pub struct OutputColumn {
    pub name: String,
    pub data_type: DataType,
    pub nullable: bool,
}

#[derive(Debug, Clone)]
pub struct BoundSelectItem {
    pub expr: BoundExpr,
    pub alias: String,
}

#[derive(Debug, Clone)]
pub enum BoundTableRef {
    BaseTable {
        table: catalog::Table,
        columns: Vec<catalog::Column>,
        alias: Option<String>,
    },
    Subquery {
        query: Box<BoundSelect>,
        alias: String,
    },
}

#[derive(Debug, Clone)]
pub struct BoundJoin {
    pub kind: BoundJoinKind,
    pub table: BoundTableRef,
    pub condition: BoundJoinCondition,
}

#[derive(Debug, Clone)]
pub enum BoundJoinKind {
    Inner,
    LeftOuter,
    RightOuter,
    FullOuter,
    Cross,
}

#[derive(Debug, Clone)]
pub enum BoundJoinCondition {
    On(BoundExpr),
    Using(Vec<BoundExpr>),
    Natural(Vec<BoundExpr>),
    None,
}

#[derive(Debug, Clone)]
pub struct BoundOrderByExpr {
    pub expr: BoundExpr,
    pub asc: bool,
    pub nulls_first: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct BoundExpr {
    pub kind: BoundExprKind,
    pub data_type: DataType,
    pub nullable: bool,
}

#[derive(Debug, Clone)]
pub enum BoundExprKind {
    Literal(BoundLiteral),
    ColumnRef(BoundColumnRef),
    BinaryOp {
        left: Box<BoundExpr>,
        op: BinaryOp,
        right: Box<BoundExpr>,
    },
    UnaryOp {
        op: UnaryOp,
        expr: Box<BoundExpr>,
    },
    IsNull(Box<BoundExpr>),
    IsNotNull(Box<BoundExpr>),
    Like {
        expr: Box<BoundExpr>,
        pattern: Box<BoundExpr>,
        negated: bool,
    },
    Between {
        expr: Box<BoundExpr>,
        low: Box<BoundExpr>,
        high: Box<BoundExpr>,
        negated: bool,
    },
    In {
        expr: Box<BoundExpr>,
        list: Vec<BoundExpr>,
        negated: bool,
    },
    InSubquery {
        expr: Box<BoundExpr>,
        subquery: Box<BoundSelect>,
        negated: bool,
    },
    Exists {
        subquery: Box<BoundSelect>,
        negated: bool,
    },
    Subquery(Box<BoundSelect>),
    Function(BoundFunction),
    Cast {
        expr: Box<BoundExpr>,
        target_type: DataType,
    },
    Case {
        operand: Option<Box<BoundExpr>>,
        when_then: Vec<(BoundExpr, BoundExpr)>,
        else_result: Option<Box<BoundExpr>>,
    },
}

#[derive(Debug, Clone)]
pub struct BoundColumnRef {
    pub qualifier: Option<String>,
    pub column_name: String,
    pub scope_index: usize,
}

#[derive(Debug, Clone)]
pub enum BoundLiteral {
    Integer(i64),
    Float(f64),
    String(String),
    Bool(bool),
    Null,
}

#[derive(Debug, Clone)]
pub struct BoundFunction {
    pub name: String,
    pub kind: FunctionKind,
    pub args: Vec<BoundExpr>,
    pub distinct: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionKind {
    Count,
    Sum,
    Avg,
    Min,
    Max,
    Coalesce,
    Nullif,
    Upper,
    Lower,
    Length,
    Abs,
    Unknown,
}

impl FunctionKind {
    pub fn is_aggregate(&self) -> bool {
        matches!(
            self,
            Self::Count | Self::Sum | Self::Avg | Self::Min | Self::Max
        )
    }
}

impl From<&str> for FunctionKind {
    fn from(s: &str) -> Self {
        match s {
            "count" => Self::Count,
            "sum" => Self::Sum,
            "avg" => Self::Avg,
            "min" => Self::Min,
            "max" => Self::Max,
            "coalesce" => Self::Coalesce,
            "nullif" => Self::Nullif,
            "upper" => Self::Upper,
            "lower" => Self::Lower,
            "length" | "len" => Self::Length,
            "abs" => Self::Abs,
            _ => Self::Unknown,
        }
    }
}
