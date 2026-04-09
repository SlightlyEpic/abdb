use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;

pub type TxnId = u64;
pub type Timestamp = u64;

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
        IsolationLevel::Snapshot
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxnState {
    Active,
    Committed(Timestamp),
    Aborted,
}

#[derive(Clone)]
pub struct Txn {
    pub id: TxnId,
    pub isolation: IsolationLevel,
    pub read_ts: Timestamp,
    pub tm: Arc<TransactionManager>,
}

pub struct TransactionManager {
    next_txn_id: AtomicU64,
    current_ts: AtomicU64,
    /// All txn ids <= this horizon are treated as committed if not in `states`.
    /// Used to recognise persisted-but-unknown xmins after restart (pre-restart
    /// transactions that survived are assumed committed).
    committed_horizon: AtomicU64,
    states: RwLock<HashMap<TxnId, TxnState>>,
    notifiers: RwLock<HashMap<TxnId, broadcast::Sender<TxnState>>>,
}

impl TransactionManager {
    pub fn new() -> Self {
        Self::with_next_txn_id(1)
    }

    pub fn with_next_txn_id(next_txn_id: TxnId) -> Self {
        Self {
            next_txn_id: AtomicU64::new(next_txn_id),
            current_ts: AtomicU64::new(next_txn_id.max(1)),
            committed_horizon: AtomicU64::new(next_txn_id.saturating_sub(1)),
            states: RwLock::new(HashMap::new()),
            notifiers: RwLock::new(HashMap::new()),
        }
    }

    pub fn begin(self: &Arc<Self>, isolation_level: IsolationLevel) -> Txn {
        let txn_id = self.next_txn_id.fetch_add(1, Ordering::SeqCst);
        let read_ts = self.current_ts.load(Ordering::SeqCst);

        {
            let mut states = self.states.write().unwrap();
            states.insert(txn_id, TxnState::Active);
        }

        Txn {
            id: txn_id,
            isolation: isolation_level,
            read_ts,
            tm: Arc::clone(self),
        }
    }

    pub fn get_txn_state(&self, txn_id: TxnId) -> TxnState {
        let states = self.states.read().unwrap();
        if let Some(&s) = states.get(&txn_id) {
            return s;
        }
        if txn_id != 0 && txn_id <= self.committed_horizon.load(Ordering::SeqCst) {
            TxnState::Committed(0)
        } else {
            TxnState::Aborted
        }
    }

    pub async fn wait_for_txn(&self, txn_id: TxnId) -> TxnState {
        let mut rx = {
            let states = self.states.read().unwrap();
            if let Some(&state) = states.get(&txn_id) {
                if state != TxnState::Active {
                    return state;
                }
            } else {
                return TxnState::Aborted;
            }

            let mut notifiers = self.notifiers.write().unwrap();
            let tx = notifiers.entry(txn_id).or_insert_with(|| broadcast::channel(1).0);
            tx.subscribe()
        };

        
        {
            let states = self.states.read().unwrap();
            if let Some(&state) = states.get(&txn_id) {
                if state != TxnState::Active {
                    return state;
                }
            }
        }

        rx.recv().await.unwrap_or(TxnState::Aborted)
    }

    pub fn commit(&self, txn: &Txn) -> crate::error::Result<()> {
        let commit_ts = self.current_ts.fetch_add(1, Ordering::SeqCst) + 1;
        self.finish_txn(txn.id, TxnState::Committed(commit_ts));
        Ok(())
    }

    pub fn rollback(&self, txn: &Txn) -> crate::error::Result<()> {
        self.finish_txn(txn.id, TxnState::Aborted);
        Ok(())
    }

    fn finish_txn(&self, txn_id: TxnId, state: TxnState) {
        {
            let mut states = self.states.write().unwrap();
            states.insert(txn_id, state);
        }
        let tx = {
            let mut notifiers = self.notifiers.write().unwrap();
            notifiers.remove(&txn_id)
        };
        if let Some(tx) = tx {
            let _ = tx.send(state);
        }
    }
}