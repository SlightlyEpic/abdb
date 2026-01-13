use crate::databox::{DataType, Value};

#[derive(Debug, Clone)]
pub enum BoundExpr {
    Constant {
        value: Value,
        data_type: DataType,
    },

    ColumnRef {
        table_name: String,
        column_name: String,
        data_type: DataType,
        column_idx: usize,
    },

    UnaryOp {
        op: UnaryOperator,
        expr: Box<BoundExpr>,
        data_type: DataType,
    },

    BinaryOp {
        op: BinaryOperator,
        left: Box<BoundExpr>,
        right: Box<BoundExpr>,
        data_type: DataType,
    },

    Star,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOperator {
    Not,
    Negate,
    IsNull,
    IsNotNull,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOperator {
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    And,
    Or,
}
