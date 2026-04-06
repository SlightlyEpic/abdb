use crate::{databox::DataType, transaction::IsolationLevel};

#[derive(Debug, Clone)]
pub struct ColumnDef {
    pub name: String,
    pub data_type: DataType,
    pub nullable: bool,
    pub default: Option<Expr>,
    pub unique: bool,
    pub primary_key: bool,
}

#[derive(Debug, Clone)]
pub enum Statement {
    BeginTransaction(IsolationLevel),
    Commit,
    Rollback,

    CreateTable(CreateTableStmt),
    DropTable(DropTableStmt),
    AlterTable(AlterTableStmt),

    Insert(InsertStmt),
    Select(SelectStmt),
    Update(UpdateStmt),
    Delete(DeleteStmt),

    CreateIndex(CreateIndexStmt),
    DropIndex(DropIndexStmt),

    Explain(Box<Statement>),
}

#[derive(Debug, Clone)]
pub struct CreateTableStmt {
    pub name: String,
    pub columns: Vec<ColumnDef>,
    pub primary_key: Vec<String>,
    pub foreign_keys: Vec<ForeignKeyClause>,
    pub if_not_exists: bool,
}

#[derive(Debug, Clone)]
pub struct ForeignKeyClause {
    pub columns: Vec<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_delete: Option<ForeignKeyAction>,
    pub on_update: Option<ForeignKeyAction>,
}

#[derive(Debug, Clone)]
pub enum ForeignKeyAction {
    NoAction,
    Cascade,
    SetNull,
    Restrict,
}

#[derive(Debug, Clone)]
pub struct DropTableStmt {
    pub name: String,
    pub if_exists: bool,
}

#[derive(Debug, Clone)]
pub struct AlterTableStmt {
    pub name: String,
    pub action: AlterTableAction,
}

#[derive(Debug, Clone)]
pub enum AlterTableAction {
    AddColumn(ColumnDef),
    DropColumn(String),
    RenameColumn { old: String, new: String },
    RenameTable(String),
    AlterColumnType { column: String, new_type: DataType },
    AddForeignKey(ForeignKeyClause),
    DropForeignKey(String),
    AddPrimaryKey(Vec<String>),
    DropConstraint(String),
}

#[derive(Debug, Clone)]
pub struct InsertStmt {
    pub table: String,
    pub columns: Option<Vec<String>>,
    pub source: InsertSource,
}

#[derive(Debug, Clone)]
pub enum InsertSource {
    Values(Vec<Vec<Expr>>),
    Select(Box<SelectStmt>),
}

#[derive(Debug, Clone)]
pub struct UpdateStmt {
    pub table: String,
    pub alias: Option<String>,
    pub assignments: Vec<Assignment>,
    pub where_clause: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct Assignment {
    pub column: String,
    pub value: Expr,
}

#[derive(Debug, Clone)]
pub struct DeleteStmt {
    pub table: String,
    pub alias: Option<String>,
    pub where_clause: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct SelectStmt {
    pub projections: Vec<SelectItem>,
    pub from: Vec<TableRef>,
    pub joins: Vec<Join>,
    pub where_clause: Option<Expr>,
    pub group_by: Vec<Expr>,
    pub having: Option<Expr>,
    pub order_by: Vec<OrderByExpr>,
    pub limit: Option<Expr>,
    pub offset: Option<Expr>,
    pub distinct: bool,
}

#[derive(Debug, Clone)]
pub enum SelectItem {
    Wildcard,
    QualifiedWildcard(String),
    Expr { expr: Expr, alias: Option<String> },
}

#[derive(Debug, Clone)]
pub enum TableRef {
    Named {
        name: String,
        alias: Option<String>,
    },
    Subquery {
        query: Box<SelectStmt>,
        alias: String,
    },
}

#[derive(Debug, Clone)]
pub struct Join {
    pub kind: JoinKind,
    pub table: TableRef,
    pub condition: JoinCondition,
}

#[derive(Debug, Clone, Copy)]
pub enum JoinKind {
    Inner,
    LeftOuter,
    RightOuter,
    FullOuter,
    Cross,
}

#[derive(Debug, Clone)]
pub enum JoinCondition {
    On(Expr),
    Using(Vec<String>),
    Natural,
    None,
}

#[derive(Debug, Clone)]
pub struct OrderByExpr {
    pub expr: Expr,
    pub asc: bool,
    pub nulls_first: Option<bool>,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Literal(Literal),
    Column(ColumnRef),
    BinaryOp {
        left: Box<Expr>,
        op: BinaryOp,
        right: Box<Expr>,
    },
    UnaryOp {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    IsNull(Box<Expr>),
    IsNotNull(Box<Expr>),
    Like {
        expr: Box<Expr>,
        pattern: Box<Expr>,
        negated: bool,
    },
    Between {
        expr: Box<Expr>,
        low: Box<Expr>,
        high: Box<Expr>,
        negated: bool,
    },
    In {
        expr: Box<Expr>,
        list: Vec<Expr>,
        negated: bool,
    },
    InSubquery {
        expr: Box<Expr>,
        subquery: Box<SelectStmt>,
        negated: bool,
    },
    Exists {
        subquery: Box<SelectStmt>,
        negated: bool,
    },
    Subquery(Box<SelectStmt>),
    Function {
        name: String,
        args: Vec<Expr>,
        distinct: bool,
    },
    Cast {
        expr: Box<Expr>,
        data_type: DataType,
    },
    Case {
        operand: Option<Box<Expr>>,
        when_then: Vec<(Expr, Expr)>,
        else_result: Option<Box<Expr>>,
    },
    Wildcard,
}

#[derive(Debug, Clone)]
pub struct ColumnRef {
    pub table: Option<String>,
    pub column: String,
}

#[derive(Debug, Clone)]
pub enum Literal {
    Integer(i64),
    Float(f64),
    String(String),
    Bool(bool),
    Null,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    And,
    Or,
    Concat,
}

#[derive(Debug, Clone)]
pub enum UnaryOp {
    Neg,
    Not,
}

#[derive(Debug, Clone)]
pub struct CreateIndexStmt {
    pub name: String,
    pub table: String,
    pub columns: Vec<String>,
    pub unique: bool,
    pub if_not_exists: bool,
}

#[derive(Debug, Clone)]
pub struct DropIndexStmt {
    pub name: String,
    pub table: String,
    pub if_exists: bool,
}
