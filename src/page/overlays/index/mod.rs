pub mod error;
pub mod btree_inner;
pub mod btree_leaf;

// Re-export main types for convenience
pub use error::OverlayError;
pub use btree_inner::{BTreeInnerEntry, BTreeInnerPage, MAX_KEYS as INNER_MAX_KEYS};
pub use btree_leaf::{BTreeLeafEntry, BTreeLeafPage, MAX_ENTRIES as LEAF_MAX_ENTRIES};
