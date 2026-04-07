use crate::common::aliases::RecordId;
use crate::databox::Value;

/// A row of values during query execution.
#[derive(Debug, Clone, PartialEq)]
pub struct Tuple {
    pub values: Vec<Value>,
    /// Optional record ID for tuples from table scans (used by Update/Delete).
    pub rid: Option<RecordId>,
}

impl Tuple {
    pub fn new(values: Vec<Value>) -> Self {
        Self { values, rid: None }
    }

    pub fn with_rid(values: Vec<Value>, rid: RecordId) -> Self {
        Self {
            values,
            rid: Some(rid),
        }
    }

    pub fn empty() -> Self {
        Self {
            values: vec![],
            rid: None,
        }
    }

    pub fn get(&self, idx: usize) -> Option<&Value> {
        self.values.get(idx)
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Concatenate two tuples (for joins).
    /// Note: RID is not preserved in joined tuples.
    pub fn concat(&self, other: &Tuple) -> Tuple {
        let mut values = self.values.clone();
        values.extend(other.values.iter().cloned());
        Tuple { values, rid: None }
    }
}

impl From<Vec<Value>> for Tuple {
    fn from(values: Vec<Value>) -> Self {
        Self::new(values)
    }
}
