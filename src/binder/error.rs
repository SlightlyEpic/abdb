use crate::databox::DataType;
use std::fmt;

#[derive(Debug)]
pub enum BindError {
    UnknownTable(String),
    UnknownColumn(String),
    AmbiguousColumn(String),
    DuplicateName(String),
    TypeMismatch {
        expected: DataType,
        found: DataType,
        context: String,
    },
    Unsupported(String),
    CatalogError(String),
    InvalidDefault(String),
    AggregateNotAllowed(String),
    NonGroupedColumn(String),
    SubqueryTooManyColumns,
    MultiColumnIndexUnsupported,
    TableAlreadyExists(String),
    IndexAlreadyExists(String),
    ColumnCountMismatch {
        expected: usize,
        found: usize,
    },
    NoColumnsInIndex,
}

impl fmt::Display for BindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTable(n) => write!(f, "unknown table: {n}"),
            Self::UnknownColumn(n) => write!(f, "unknown column: {n}"),
            Self::AmbiguousColumn(n) => write!(f, "ambiguous column reference: {n}"),
            Self::DuplicateName(n) => write!(f, "duplicate name: {n}"),
            Self::TypeMismatch {
                expected,
                found,
                context,
            } => write!(
                f,
                "type mismatch in {context}: expected {expected:?}, found {found:?}"
            ),
            Self::Unsupported(s) => write!(f, "unsupported: {s}"),
            Self::CatalogError(s) => write!(f, "catalog error: {s}"),
            Self::InvalidDefault(s) => write!(f, "invalid default: {s}"),
            Self::AggregateNotAllowed(s) => write!(f, "aggregate not allowed here: {s}"),
            Self::NonGroupedColumn(s) => write!(f, "column must appear in GROUP BY: {s}"),
            Self::SubqueryTooManyColumns => {
                write!(f, "scalar subquery must return exactly one column")
            }
            Self::MultiColumnIndexUnsupported => {
                write!(f, "multi-column indexes are not yet supported")
            }
            Self::TableAlreadyExists(n) => write!(f, "table already exists: {n}"),
            Self::IndexAlreadyExists(n) => write!(f, "index already exists: {n}"),
            Self::ColumnCountMismatch { expected, found } => write!(
                f,
                "column count mismatch: expected {expected}, found {found}"
            ),
            Self::NoColumnsInIndex => write!(f, "index must specify at least one column"),
        }
    }
}

impl std::error::Error for BindError {}

pub type BindResult<T> = std::result::Result<T, BindError>;
