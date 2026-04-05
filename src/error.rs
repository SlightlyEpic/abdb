use thiserror::Error;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("parse error: {0}")]
    Parse(String),

    #[error("bind error: {0}")]
    Bind(#[from] crate::binder::BindError),

    #[error("cannot execute multiple statements at once")]
    TooManyStatements,

    #[error("no statement to execute")]
    EmptyStatement,

    #[error("transaction already in progress")]
    TransactionAlreadyInProgress,

    #[error("txn: {0}")]
    InvalidTransactionState(String),
    
    #[error("not in transaction")]
    NotInTransaction
}

pub type Result<T> = std::result::Result<T, DbError>;
