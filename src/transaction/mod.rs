use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

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
    /// Set of currently active transaction IDs.
    active_txns: RwLock<HashSet<TxnId>>,
}

impl TransactionManager {
    pub fn new() -> Self {
        Self::with_next_txn_id(1)
    }

    pub fn with_next_txn_id(next_txn_id: TxnId) -> Self {
        Self {
            next_txn_id: AtomicU64::new(next_txn_id),
            current_ts: AtomicU64::new(0),
            active_txns: RwLock::new(HashSet::new()),
        }
    }

    pub fn begin(&self, isolation_level: IsolationLevel) -> Transaction {
        let txn_id = self.next_txn_id.fetch_add(1, Ordering::SeqCst);
        let read_ts = self.current_ts.load(Ordering::SeqCst);

        // Add to active transactions
        {
            let mut active = self.active_txns.write().expect("active_txns lock poisoned");
            active.insert(txn_id);
        }

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

    /// Commit a transaction.
    ///
    /// Assigns a commit timestamp and marks the transaction as committed.
    /// The commit timestamp is used for visibility: tuples created by this
    /// transaction become visible to transactions that start after this commit.
    pub fn commit(&self, txn: &mut Transaction) -> Result<()> {
        if txn.state != TxnState::Active {
            return Err(crate::error::DbError::InvalidTransactionState(format!(
                "Cannot commit transaction in state {:?}",
                txn.state
            )));
        }

        // Assign commit timestamp and advance global clock
        let commit_ts = self.current_ts.fetch_add(1, Ordering::SeqCst) + 1;
        txn.commit_ts = Some(commit_ts);
        txn.state = TxnState::Committed;

        // Remove from active transactions
        {
            let mut active = self.active_txns.write().expect("active_txns lock poisoned");
            active.remove(&txn.txn_id);
        }

        println!(
            "[txn] commit txn_id={} commit_ts={}",
            txn.txn_id, commit_ts
        );

        Ok(())
    }

    /// Abort/rollback a transaction.
    ///
    /// Marks the transaction as aborted. Any tuples created by this transaction
    /// should not be visible to other transactions.
    ///
    /// Note: In a full implementation, this would also need to undo any writes
    /// made by the transaction (either via undo logs or by marking tuples with
    /// XMAX). For now, we just mark the state.
    pub fn rollback(&self, txn: &mut Transaction) -> Result<()> {
        if txn.state != TxnState::Active {
            return Err(crate::error::DbError::InvalidTransactionState(format!(
                "Cannot rollback transaction in state {:?}",
                txn.state
            )));
        }

        txn.state = TxnState::Aborted;

        // Remove from active transactions
        {
            let mut active = self.active_txns.write().expect("active_txns lock poisoned");
            active.remove(&txn.txn_id);
        }

        println!("[txn] rollback txn_id={}", txn.txn_id);

        Ok(())
    }

    /// Check if a transaction is currently active.
    pub fn is_active(&self, txn_id: TxnId) -> bool {
        let active = self.active_txns.read().expect("active_txns lock poisoned");
        active.contains(&txn_id)
    }

    /// Get the set of currently active transaction IDs.
    /// Used for snapshot isolation to determine visibility.
    pub fn active_transaction_ids(&self) -> HashSet<TxnId> {
        let active = self.active_txns.read().expect("active_txns lock poisoned");
        active.clone()
    }
}
