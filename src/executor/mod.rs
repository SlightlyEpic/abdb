// src/executor/mod.rs

pub mod error;
pub mod operators;
pub mod traits;

pub use error::ExecutorError;
pub use traits::*;

// Re-export commonly used types
pub type ExecutorResult<T> = Result<T, ExecutorError>;
