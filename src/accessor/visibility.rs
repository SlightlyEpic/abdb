use crate::common::aliases::TxnId;
use crate::databox::HEADER_SIZE;
use crate::transaction::{IsolationLevel, Txn, TxnState};

pub const TUPLE_HEADER_SIZE: usize = HEADER_SIZE as usize;

#[inline]
pub fn read_xmin(tuple: &[u8]) -> TxnId {
    let bytes: [u8; 8] = tuple[0..8].try_into().expect("tuple too short for XMIN");
    u64::from_le_bytes(bytes)
}

#[inline]
pub fn read_xmax(tuple: &[u8]) -> TxnId {
    let bytes: [u8; 8] = tuple[8..16].try_into().expect("tuple too short for XMAX");
    u64::from_le_bytes(bytes)
}

#[inline]
pub fn write_xmin(tuple: &mut [u8], xmin: TxnId) {
    tuple[0..8].copy_from_slice(&xmin.to_le_bytes());
}

#[inline]
pub fn write_xmax(tuple: &mut [u8], xmax: TxnId) {
    tuple[8..16].copy_from_slice(&xmax.to_le_bytes());
}

#[inline]
pub fn make_header(xmin: TxnId) -> [u8; 16] {
    let mut header = [0u8; 16];
    header[0..8].copy_from_slice(&xmin.to_le_bytes());
    header
}

pub fn is_visible_mvcc(xmin: TxnId, xmax: TxnId, txn: &Txn) -> bool {
    let xmin_visible = if xmin == 0 {
        true
    } else if xmin == txn.id {
        true
    } else {
        match txn.tm.get_txn_state(xmin) {
            TxnState::Committed(ts) => {
                if txn.isolation == IsolationLevel::ReadUncommitted {
                    true
                } else {
                    ts <= txn.read_ts
                }
            }
            TxnState::Active => txn.isolation == IsolationLevel::ReadUncommitted,
            TxnState::Aborted => false,
        }
    };

    if !xmin_visible {
        return false;
    }

    if xmax == 0 {
        return true;
    }
    if xmax == txn.id {
        return false;
    }

    match txn.tm.get_txn_state(xmax) {
        TxnState::Committed(ts) => {
            if txn.isolation == IsolationLevel::ReadUncommitted {
                false
            } else {
                ts > txn.read_ts
            }
        }
        TxnState::Active => txn.isolation != IsolationLevel::ReadUncommitted,
        TxnState::Aborted => true,
    }
}

pub fn is_visible(tuple: &[u8], txn: &Txn) -> bool {
    if tuple.len() < TUPLE_HEADER_SIZE {
        return false;
    }

    let xmin = read_xmin(tuple);
    let xmax = read_xmax(tuple);

    is_visible_mvcc(xmin, xmax, txn)
}
