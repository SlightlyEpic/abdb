// visibility.rs should be moved here i guess

use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::Result;

pub type TxnId = u64;
pub type Timestamp = u64;
pub type RowId = u64;

pub const PENDING_TS: Timestamp = u64::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationLevel {
    ReadUncommitted,
    ReadCommitted,
    RepeatableRead,
    Snapshot,
    Serializable,
}

impl IsolationLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            IsolationLevel::ReadUncommitted => "READ UNCOMMITTED",
            IsolationLevel::ReadCommitted => "READ COMMITTED",
            IsolationLevel::RepeatableRead => "REPEATABLE READ",
            IsolationLevel::Snapshot => "SNAPSHOT",
            IsolationLevel::Serializable => "SERIALIZABLE",
        }
    }
}

impl Default for IsolationLevel {
    fn default() -> Self {
        IsolationLevel::RepeatableRead
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TxnState {
    Active,
    Committed,
    Aborted,
}

#[derive(Debug)]
pub struct Transaction {
    pub txn_id: TxnId,
    pub read_ts: Timestamp,
    pub commit_ts: Option<Timestamp>,
    pub state: TxnState,
    pub isolation_level: IsolationLevel,
}

impl Transaction {
    pub fn new(txn_id: TxnId, read_ts: Timestamp, isolation_level: IsolationLevel) -> Self {
        Self {
            txn_id,
            read_ts,
            commit_ts: None,
            state: TxnState::Active,
            isolation_level,
        }
    }
}

pub struct TransactionManager {
    next_txn_id: AtomicU64,
    current_ts: AtomicU64,
}

impl TransactionManager {
    pub fn new() -> Self {
        Self {
            next_txn_id: AtomicU64::new(1),
            current_ts: AtomicU64::new(0),
        }
    }

    pub fn begin(&self, isolation_level: IsolationLevel) -> Transaction {
        let txn_id = self.next_txn_id.fetch_add(1, Ordering::SeqCst);
        let read_ts = self.current_ts.load(Ordering::SeqCst);
        println!(
            "[txn] begin txn_id={} read_ts={} isolation={}",
            txn_id,
            read_ts,
            isolation_level.as_str()
        );
        Transaction::new(txn_id, read_ts, isolation_level)
    }

    pub fn current_ts(&self) -> Timestamp {
        self.current_ts.load(Ordering::SeqCst)
    }

    pub fn commit(&self, _txn: &mut Transaction) -> Result<()> {
        todo!();
    }
}
