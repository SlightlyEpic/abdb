use crate::page::PageType;

/// Errors that can occur during index overlay operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayError {
    /// Page type does not match expected type for this overlay.
    TypeMismatch {
        expected: PageType,
        found: PageType,
    },

    /// Invalid page type byte encountered (not a valid PageType variant).
    InvalidPageType(u8),

    /// Page has reached maximum capacity.
    PageFull {
        capacity: usize,
        attempted: usize,
    },

    /// Attempted to insert a key that already exists.
    DuplicateKey { key: u64 },

    /// Entry index is out of bounds.
    IndexOutOfBounds { index: u16, max: u16 },

    /// Reserved for future MVCC support: transaction visibility error.
    TransactionVisibility {
        entry_txn: u64,
        current_txn: u64,
    },
}

impl std::fmt::Display for OverlayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TypeMismatch { expected, found } => {
                write!(
                    f,
                    "Page type mismatch: expected {:?}, found {:?}",
                    expected, found
                )
            }
            Self::InvalidPageType(byte) => {
                write!(f, "Invalid page type byte: {}", byte)
            }
            Self::PageFull {
                capacity,
                attempted,
            } => {
                write!(
                    f,
                    "Page full: capacity is {}, attempted to add entry #{}",
                    capacity, attempted
                )
            }
            Self::DuplicateKey { key } => {
                write!(f, "Duplicate key: {} already exists", key)
            }
            Self::IndexOutOfBounds { index, max } => {
                write!(f, "Index {} out of bounds (max: {})", index, max)
            }
            Self::TransactionVisibility {
                entry_txn,
                current_txn,
            } => {
                write!(
                    f,
                    "Entry from transaction {} not visible to transaction {}",
                    entry_txn, current_txn
                )
            }
        }
    }
}

impl std::error::Error for OverlayError {}
