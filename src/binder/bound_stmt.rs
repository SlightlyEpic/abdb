use super::{BoundExpr, DataType};

#[derive(Debug, Clone)]
pub struct ColumnDef {
    pub name: String,
    pub data_type: DataType,
    pub nullable: bool,
    pub default: Option<BoundExpr>,
}

#[derive(Debug)]
pub struct BoundCreateTable {
    pub table_name: String,
    pub columns: Vec<ColumnDef>,
    pub primary_key: Vec<String>,
}

#[derive(Debug)]
pub struct BoundInsert {
    pub table_name: String,
    pub table_oid: u32,
    pub columns: Vec<usize>,
    pub values: Vec<Vec<BoundExpr>>,
}

#[derive(Debug)]
pub struct BoundUpdate {
    pub table_name: String,
    pub table_oid: u32,
    pub assignments: Vec<(usize, BoundExpr)>,
    pub where_clause: Option<BoundExpr>,
}

#[derive(Debug)]
pub struct BoundDelete {
    pub table_name: String,
    pub table_oid: u32,
    pub where_clause: Option<BoundExpr>,
}

#[derive(Debug)]
pub struct BoundSelect {
    pub select_list: Vec<BoundExpr>,
    pub from: BoundTableRef,
    pub where_clause: Option<BoundExpr>,
    pub group_by: Vec<BoundExpr>, // in case we support these
    pub having: Option<BoundExpr>,
    pub order_by: Vec<(BoundExpr, bool)>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug)]
pub enum BoundTableRef {
    BaseTable {
        table_name: String,
        table_oid: u32,
        alias: Option<String>,
    },

    Join {
        join_type: JoinType,
        left: Box<BoundTableRef>,
        right: Box<BoundTableRef>,
        condition: Option<BoundExpr>,
    },

    CrossProduct {
        tables: Vec<BoundTableRef>,
    },

    // will we support it?
    Subquery {
        query: Box<BoundSelect>,
        alias: String,
    },
    // todo
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinType {
    Inner,
    Left,
    Right,
    Full,
    Cross,
}

#[derive(Debug)]
pub enum BoundStatement {
    CreateTable(BoundCreateTable),
    Insert(BoundInsert),
    Update(BoundUpdate),
    Delete(BoundDelete),
    Select(BoundSelect),
}
