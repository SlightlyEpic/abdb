mod evaluate;
mod execute;
mod tuple;

pub use evaluate::{evaluate_expr, evaluate_predicate};
pub use execute::{execute, ExecutionResult};
pub use tuple::Tuple;
