use thiserror::Error;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("parse error: {0}")]
    Parse(String),

    #[error("bind error: {0}")]
    Bind(#[from] crate::binder::BindError),
}

pub type Result<T> = std::result::Result<T, DbError>;
