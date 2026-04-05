use std::sync::{Arc, Mutex};

use crate::accessor::AccessorImpl;
use crate::buffer::r#impl::BufferPool;
use crate::common::aliases::OId;
use crate::error::Result;
use crate::storage::DiskManagerImpl;
use crate::storage::allocator::SimpleAllocator;
use crate::storage::directory::BTreePageDirectory;
use crate::transaction::{IsolationLevel, Transaction, TransactionManager};

#[derive(Debug, PartialEq)]
pub enum TransactionState {
    Idle,
    InTransaction,
    Failed,
}

pub struct Session {
    pub session_id: u64,
    pub txn_state: TransactionState,
    pub current_txn: Option<Transaction>,
    pub session_isolation_level: IsolationLevel,
    pub next_txn_isolation_level: Option<IsolationLevel>,

    accessor: Arc<AccessorImpl<BufferPool<DiskManagerImpl<BTreePageDirectory, SimpleAllocator>>>>,
    txn_manager: Arc<TransactionManager>,

    next_oid: OId,
}

static SESSION_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl Session {
    pub fn new(
        accessor: Arc<
            AccessorImpl<BufferPool<DiskManagerImpl<BTreePageDirectory, SimpleAllocator>>>,
        >,
        txn_manager: Arc<TransactionManager>,
    ) -> Self {
        let session_id = SESSION_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self {
            session_id,
            txn_state: TransactionState::Idle,
            current_txn: None,
            session_isolation_level: IsolationLevel::default(),
            next_txn_isolation_level: None,
            accessor,
            txn_manager,
            next_oid: 1,
        }
    }

    pub fn execute_sql(&mut self, sql: &str) -> Result<String> {
        todo!()
    }
}
