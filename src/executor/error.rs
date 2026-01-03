use std::fmt;

/// Executor-specific errors
#[derive(Debug, Clone)]
pub enum ExecutorError {
    /// Storage layer errors (wrapped from storage)
    StorageError(String),

    /// Transaction-related errors
    TransactionConflict(String),
    TransactionAborted,

    /// Schema and constraint violations
    SchemaError(String),
    ConstraintViolation(String),

    /// Data type errors during execution
    TypeError(String),

    /// Index-related errors
    IndexError(String),

    /// Row not found errors
    RowNotFound(String),

    /// I/O errors
    IoError(String),

    /// Plan generation errors
    PlanError(String),

    /// Evaluation errors (expression evaluation, etc.)
    EvaluationError(String),

    /// Internal executor errors
    InternalError(String),
}

impl fmt::Display for ExecutorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecutorError::StorageError(msg) => write!(f, "Storage error: {}", msg),
            ExecutorError::TransactionConflict(msg) => write!(f, "Transaction conflict: {}", msg),
            ExecutorError::TransactionAborted => write!(f, "Transaction aborted"),
            ExecutorError::SchemaError(msg) => write!(f, "Schema error: {}", msg),
            ExecutorError::ConstraintViolation(msg) => write!(f, "Constraint violation: {}", msg),
            ExecutorError::TypeError(msg) => write!(f, "Type error: {}", msg),
            ExecutorError::IndexError(msg) => write!(f, "Index error: {}", msg),
            ExecutorError::RowNotFound(msg) => write!(f, "Row not found: {}", msg),
            ExecutorError::IoError(msg) => write!(f, "I/O error: {}", msg),
            ExecutorError::PlanError(msg) => write!(f, "Plan error: {}", msg),
            ExecutorError::EvaluationError(msg) => write!(f, "Evaluation error: {}", msg),
            ExecutorError::InternalError(msg) => write!(f, "Internal error: {}", msg),
        }
    }
}

impl std::error::Error for ExecutorError {}

impl From<std::io::Error> for ExecutorError {
    fn from(err: std::io::Error) -> Self {
        ExecutorError::IoError(err.to_string())
    }
}
