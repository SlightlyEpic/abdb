use crate::databox::Value;

/// A row of values during query execution.
#[derive(Debug, Clone, PartialEq)]
pub struct Tuple {
    pub values: Vec<Value>,
}

impl Tuple {
    pub fn new(values: Vec<Value>) -> Self {
        Self { values }
    }

    pub fn empty() -> Self {
        Self { values: vec![] }
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
    pub fn concat(&self, other: &Tuple) -> Tuple {
        let mut values = self.values.clone();
        values.extend(other.values.iter().cloned());
        Tuple { values }
    }
}

impl From<Vec<Value>> for Tuple {
    fn from(values: Vec<Value>) -> Self {
        Self::new(values)
    }
}
