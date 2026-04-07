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
    NotInTransaction,

    #[error("column index out of bounds: {0}")]
    ColumnIndexOutOfBounds(usize),

    #[error("cast error: {0}")]
    CastError(String),

    #[error("type mismatch: {0}")]
    TypeMismatch(String),

    #[error("division by zero")]
    DivisionByZero,

    #[error("invalid arguments: {0}")]
    InvalidArguments(String),

    #[error("unsupported: {0}")]
    Unsupported(String),

    #[error("internal error: {0}")]
    Internal(String),

    #[error("accessor error: {0}")]
    AccessorError(String),

    #[error("plan error: {0}")]
    PlanError(String),

    #[error("optimizer error: {0}")]
    OptimizerError(String),
}

pub type Result<T> = std::result::Result<T, DbError>;
