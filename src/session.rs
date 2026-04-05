use std::sync::Arc;

use crate::accessor::AccessorImpl;
use crate::buffer::r#impl::BufferPool;
use crate::common::aliases::OId;
use crate::error::{DbError, Result};
use crate::storage::DiskManagerImpl;
use crate::storage::allocator::SimpleAllocator;
use crate::storage::directory::BTreePageDirectory;
use crate::transaction::{IsolationLevel, Transaction, TransactionManager};

use crate::parser::{self, ast::Statement};

pub const DEFAULT_ISOLATION_LEVEL: IsolationLevel = IsolationLevel::Snapshot;

pub struct Session {
    pub session_id: u64,
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
            current_txn: None,
            session_isolation_level: IsolationLevel::default(),
            next_txn_isolation_level: None,
            accessor,
            txn_manager,
            next_oid: 1,
        }
    }

    pub fn execute_sql(&mut self, sql: &str) -> Result<String> {
        let ast = parser::Parser::parse(sql)?;

        let stmt = match ast.as_slice() {
            [stmt] => stmt.to_owned(),
            []     => return Err(DbError::EmptyStatement),
            _      => return Err(DbError::TooManyStatements),
        };

        use parser::ast::*;

        match stmt {
            Statement::BeginTransaction(isolation_level) => {
                if self.current_txn.is_some() {
                    Err(DbError::TransactionAlreadyInProgress)
                } else {
                    self.current_txn = Some(self.txn_manager.begin(isolation_level));
                    Ok("BEGIN".into())
                }
            }
            Statement::Commit => {
                let current_txn = self.current_txn.as_mut().ok_or(DbError::NotInTransaction)?;
                self.txn_manager.commit(current_txn)?;
                self.current_txn = None;
                Ok("COMMIT".into())
            }
            Statement::Rollback => {
                let current_txn = self.current_txn.as_mut().ok_or(DbError::NotInTransaction)?;
                self.txn_manager.rollback(current_txn)?;
                self.current_txn = None;
                Ok("ROLLBACK".into())
            }
            other => self.execute_sql_in_txn(other)
        }
    }

    pub fn execute_sql_in_txn(&mut self, stmt: Statement) -> Result<String> {
        // let binder = Binder::new(&mut self.current_txn, &self.accessor);
        Ok("".into())
    }
}
