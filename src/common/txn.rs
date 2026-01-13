use crate::common::aliases::TxnId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationLevel {
    ReadUncommitted,
    ReadCommitted,
    Snapshot,
}

#[derive(Debug, Clone, Copy)]
pub struct Txn {
    pub id: TxnId,
    pub isolation: IsolationLevel,
}
