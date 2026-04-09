use crate::databox::Value;

#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Row {
    pub values: Vec<Value>,
}

#[derive(Debug, Clone)]
pub enum Response {
    Rows {
        columns: Vec<Column>,
        rows: Vec<Row>,
    },
    RowsAffected(u64),
    Notice(String),
    BeginTransaction,
    Commit,
    Rollback,
}

impl Response {
    pub fn notice(msg: impl Into<String>) -> Self {
        Self::Notice(msg.into())
    }
}