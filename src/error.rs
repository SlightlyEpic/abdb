use thiserror::Error;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("parse error: {0}")]
    Parse(String),
}

pub type Result<T> = std::result::Result<T, DbError>;
