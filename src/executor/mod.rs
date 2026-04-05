mod codec;
mod eval;
mod executor;

pub use executor::Executor;

use crate::databox::Value;
use std::fmt;

pub type Row = Vec<Value>;

#[derive(Debug)]
pub enum ExecError {
    Accessor(crate::accessor::Error),
    Eval(String),
    Internal(String),
}

impl From<crate::accessor::Error> for ExecError {
    fn from(e: crate::accessor::Error) -> Self {
        ExecError::Accessor(e)
    }
}

impl fmt::Display for ExecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecError::Accessor(e) => write!(f, "accessor: {:?}", e),
            ExecError::Eval(s) => write!(f, "eval: {s}"),
            ExecError::Internal(s) => write!(f, "internal: {s}"),
        }
    }
}

impl std::error::Error for ExecError {}

pub type ExecResult<T> = std::result::Result<T, ExecError>;
