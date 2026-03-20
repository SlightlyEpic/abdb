use crate::common::{aliases::TxnId, txn::{IsolationLevel, Txn}};
use crate::databox::HEADER_SIZE;

/// Size of the MVCC tuple header in bytes (u64 XMIN + u64 XMAX).
pub const TUPLE_HEADER_SIZE: usize = HEADER_SIZE as usize;

/// Read the XMIN (creating transaction) from a raw tuple.
#[inline]
pub fn read_xmin(tuple: &[u8]) -> TxnId {
    let bytes: [u8; 8] = tuple[0..8]
        .try_into()
        .expect("tuple too short for XMIN");
    u64::from_le_bytes(bytes)
}

/// Read the XMAX (deleting transaction) from a raw tuple.
/// Returns 0 if the tuple has not been deleted.
#[inline]
pub fn read_xmax(tuple: &[u8]) -> TxnId {
    let bytes: [u8; 8] = tuple[8..16]
        .try_into()
        .expect("tuple too short for XMAX");
    u64::from_le_bytes(bytes)
}

/// Write XMIN into the tuple header.
#[inline]
pub fn write_xmin(tuple: &mut [u8], xmin: TxnId) {
    tuple[0..8].copy_from_slice(&xmin.to_le_bytes());
}

/// Write XMAX into the tuple header.
#[inline]
pub fn write_xmax(tuple: &mut [u8], xmax: TxnId) {
    tuple[8..16].copy_from_slice(&xmax.to_le_bytes());
}

/// Build a tuple header (XMIN + XMAX) as a 16-byte array.
#[inline]
pub fn make_header(xmin: TxnId) -> [u8; 16] {
    let mut header = [0u8; 16];
    header[0..8].copy_from_slice(&xmin.to_le_bytes());
    // XMAX = 0 (not deleted)
    header
}

/// Check whether a tuple is visible to the given transaction.
///
/// Visibility rules:
/// - **ReadUncommitted**: always visible (dirty reads)
/// - **ReadCommitted / Snapshot**:
///   - XMIN <= txn.id (tuple was created by a committed-or-current txn)
///   - AND XMAX == 0 (not deleted) OR XMAX > txn.id (deleted by a future txn)
pub fn is_visible(tuple: &[u8], txn: &Txn) -> bool {
    if tuple.len() < TUPLE_HEADER_SIZE {
        return false;
    }
    match txn.isolation {
        IsolationLevel::ReadUncommitted => true,
        IsolationLevel::ReadCommitted | IsolationLevel::Snapshot => {
            let xmin = read_xmin(tuple);
            let xmax = read_xmax(tuple);
            xmin <= txn.id && (xmax == 0 || xmax > txn.id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::txn::Txn;

    fn make_txn(id: TxnId) -> Txn {
        Txn {
            id,
            isolation: IsolationLevel::ReadCommitted,
        }
    }

    #[test]
    fn test_visibility_live_tuple() {
        let mut tuple = vec![0u8; 32];
        write_xmin(&mut tuple, 5);
        // XMAX = 0 (not deleted)

        assert!(is_visible(&tuple, &make_txn(5)));
        assert!(is_visible(&tuple, &make_txn(10)));
        assert!(!is_visible(&tuple, &make_txn(3)));
    }

    #[test]
    fn test_visibility_deleted_tuple() {
        let mut tuple = vec![0u8; 32];
        write_xmin(&mut tuple, 5);
        write_xmax(&mut tuple, 10);

        // Visible to txn 7 (between xmin and xmax)
        assert!(is_visible(&tuple, &make_txn(7)));
        // Not visible to txn 10 (xmax <= txn.id)
        assert!(!is_visible(&tuple, &make_txn(10)));
        // Not visible to txn 15
        assert!(!is_visible(&tuple, &make_txn(15)));
    }

    #[test]
    fn test_read_uncommitted_always_visible() {
        let mut tuple = vec![0u8; 32];
        write_xmin(&mut tuple, 100);
        write_xmax(&mut tuple, 5);

        let txn = Txn {
            id: 1,
            isolation: IsolationLevel::ReadUncommitted,
        };
        assert!(is_visible(&tuple, &txn));
    }

    #[test]
    fn test_make_header() {
        let header = make_header(42);
        assert_eq!(read_xmin(&header), 42);
        assert_eq!(read_xmax(&header), 0);
    }
}
