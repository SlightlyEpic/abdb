use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::accessor::AccessorImpl;
use crate::binder::{Binder, OidAllocator};
use crate::buffer::r#impl::BufferPool;
use crate::error::{DbError, Result};
use crate::executor::{self, ExecutionResult};
use crate::optimizer::Optimizer;
use crate::parser::{self, ast::Statement};
use crate::planner::Planner;
use crate::response::{Column, Response, Row};
use crate::storage::DiskManagerImpl;
use crate::storage::allocator::SimpleAllocator;
use crate::storage::directory::BTreePageDirectory;
use crate::transaction::{IsolationLevel, TransactionManager, Txn};

type DM = DiskManagerImpl<BTreePageDirectory, SimpleAllocator>;
type BP = BufferPool<DM>;
type Acc = AccessorImpl<BP>;

pub struct Session {
    pub session_id: u64,
    pub current_txn: Option<Txn>,
    pub session_isolation_level: IsolationLevel,

    accessor: Arc<Acc>,
    txn_manager: Arc<TransactionManager>,
    oid_allocator: Arc<dyn OidAllocator>,
}

static SESSION_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl Session {
    pub fn new(
        accessor: Arc<Acc>,
        txn_manager: Arc<TransactionManager>,
        oid_allocator: Arc<dyn OidAllocator>,
    ) -> Self {
        let session_id = SESSION_COUNTER.fetch_add(1, Ordering::SeqCst);
        Self {
            session_id,
            current_txn: None,
            session_isolation_level: IsolationLevel::default(),
            accessor,
            txn_manager,
            oid_allocator,
        }
    }

    pub async fn execute_sql(&mut self, sql: &str) -> Vec<Result<Response>> {
        let stmts = match parser::Parser::parse(sql) {
            Ok(v) => v,
            Err(e) => return vec![Err(e)],
        };

        if stmts.is_empty() {
            return vec![];
        }

        let mut results = Vec::with_capacity(stmts.len());
        for stmt in stmts {
            let r = self.execute_one(stmt).await;
            let is_err = r.is_err();
            results.push(r);
            if is_err {
                break;
            }
        }
        results
    }

    async fn execute_one(&mut self, stmt: Statement) -> Result<Response> {
        use parser::ast::Statement::*;
        match stmt {
            BeginTransaction(isolation_level) => {
                if self.current_txn.is_some() {
                    return Err(DbError::TransactionAlreadyInProgress);
                }
                self.current_txn = Some(
                    self.txn_manager
                        .begin(isolation_level.unwrap_or(self.session_isolation_level)),
                );
                Ok(Response::BeginTransaction)
            }
            Commit => {
                let txn = self.current_txn.take().ok_or(DbError::NotInTransaction)?;
                if self.txn_manager.get_txn_state(txn.id)
                    == crate::transaction::TxnState::Aborted
                {
                    return Ok(Response::Rollback);
                }
                self.txn_manager.commit(&txn)?;
                self.accessor
                    .flush()
                    .await
                    .map_err(|e| DbError::Internal(format!("flush on commit failed: {:?}", e)))?;
                Ok(Response::Commit)
            }
            Rollback => {
                let txn = self.current_txn.take().ok_or(DbError::NotInTransaction)?;
                let _ = self.txn_manager.rollback(&txn);
                Ok(Response::Rollback)
            }
            other => self.execute_in_txn(other).await,
        }
    }

    async fn execute_in_txn(&mut self, stmt: Statement) -> Result<Response> {
        let is_auto_commit = self.current_txn.is_none();
        let txn = self.get_or_begin_txn();

        if !is_auto_commit
            && self.txn_manager.get_txn_state(txn.id) == crate::transaction::TxnState::Aborted
        {
            return Err(DbError::InvalidTransactionState(
                "current transaction is aborted, commands ignored until end of transaction block"
                    .into(),
            ));
        }

        let result = async {
            let binder = Binder::new(
                Arc::clone(&self.accessor),
                Arc::clone(&self.oid_allocator),
                txn.clone(),
            );
            let bound = binder.bind(stmt)?;
            let plan = Planner::plan(bound)?;
            let optimizer = Optimizer::new(Arc::clone(&self.accessor), txn.clone());
            let physical = optimizer.optimize(plan)?;
            executor::execute(physical, self.accessor.as_ref(), txn.clone()).await
        }
        .await;

        match result {
            Ok(exec_result) => {
                if is_auto_commit {
                    self.txn_manager.commit(&txn)?;
                    self.accessor.flush().await.map_err(|e| {
                        DbError::Internal(format!("flush after auto-commit failed: {:?}", e))
                    })?;
                }
                Ok(execution_result_to_response(exec_result))
            }
            Err(e) => {
                let _ = self.txn_manager.rollback(&txn);
                Err(e)
            }
        }
    }

    fn get_or_begin_txn(&mut self) -> Txn {
        match &self.current_txn {
            Some(t) => t.clone(),
            None => {
                let isolation = self.session_isolation_level;
                self.txn_manager.begin(isolation)
            }
        }
    }
}

fn execution_result_to_response(result: ExecutionResult) -> Response {
    match result {
        ExecutionResult::Ok(msg) => Response::Notice(msg),
        ExecutionResult::RowsAffected(n) => Response::RowsAffected(n),
        ExecutionResult::Rows { columns, rows } => {
            let cols = columns.into_iter().map(|name| Column { name }).collect();
            let rs = rows
                .into_iter()
                .map(|t| Row { values: t.values })
                .collect();
            Response::Rows {
                columns: cols,
                rows: rs,
            }
        }
    }
}