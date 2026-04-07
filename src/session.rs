use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use crate::accessor::AccessorImpl;
use crate::binder::{Binder, OidAllocator};
use crate::buffer::r#impl::BufferPool;
use crate::common::aliases::{FileId, OId};
use crate::common::txn::Txn;
use crate::error::{DbError, Result};
use crate::executor::{self, ExecutionResult};
use crate::optimizer::Optimizer;
use crate::parser::{self, ast::Statement};
use crate::planner::Planner;
use crate::storage::allocator::SimpleAllocator;
use crate::storage::directory::BTreePageDirectory;
use crate::storage::DiskManagerImpl;
use crate::transaction::{IsolationLevel, Transaction, TransactionManager};

pub const DEFAULT_ISOLATION_LEVEL: IsolationLevel = IsolationLevel::Snapshot;

type DM = DiskManagerImpl<BTreePageDirectory, SimpleAllocator>;
type BP = BufferPool<DM>;
type Acc = AccessorImpl<BP>;

/// OID allocator for the session.
struct SessionOidAllocator {
    next_oid: AtomicU32,
    next_file_id: AtomicU32,
}

impl SessionOidAllocator {
    fn new(start_oid: u32, start_file_id: u32) -> Self {
        Self {
            next_oid: AtomicU32::new(start_oid),
            next_file_id: AtomicU32::new(start_file_id),
        }
    }
}

impl OidAllocator for SessionOidAllocator {
    fn next_oid(&self) -> OId {
        self.next_oid.fetch_add(1, Ordering::SeqCst)
    }

    fn next_file_id(&self) -> FileId {
        self.next_file_id.fetch_add(1, Ordering::SeqCst)
    }
}

pub struct Session {
    pub session_id: u64,
    pub current_txn: Option<Transaction>,
    pub session_isolation_level: IsolationLevel,
    pub next_txn_isolation_level: Option<IsolationLevel>,

    accessor: Arc<Acc>,
    txn_manager: Arc<TransactionManager>,
    oid_allocator: Arc<SessionOidAllocator>,
}

static SESSION_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl Session {
    pub fn new(accessor: Arc<Acc>, txn_manager: Arc<TransactionManager>) -> Self {
        let session_id = SESSION_COUNTER.fetch_add(1, Ordering::SeqCst);
        Self {
            session_id,
            current_txn: None,
            session_isolation_level: IsolationLevel::default(),
            next_txn_isolation_level: None,
            accessor,
            txn_manager,
            oid_allocator: Arc::new(SessionOidAllocator::new(1000, 100)),
        }
    }

    pub fn execute_sql(&mut self, sql: &str) -> Result<String> {
        let ast = parser::Parser::parse(sql)?;

        let stmt = match ast.as_slice() {
            [stmt] => stmt.to_owned(),
            [] => return Err(DbError::EmptyStatement),
            _ => return Err(DbError::TooManyStatements),
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
            other => {
                // Use tokio runtime to execute async code
                let rt = tokio::runtime::Handle::try_current()
                    .unwrap_or_else(|_| {
                        tokio::runtime::Runtime::new().unwrap().handle().clone()
                    });
                rt.block_on(self.execute_sql_in_txn(other))
            }
        }
    }

    async fn execute_sql_in_txn(&mut self, stmt: Statement) -> Result<String> {
        // Get or create transaction
        let txn = self.get_or_begin_txn();

        // 1. Bind
        let binder = Binder::new(
            Arc::clone(&self.accessor),
            Arc::clone(&self.oid_allocator),
            txn,
        );
        let bound = binder.bind(stmt)?;

        // 2. Plan
        let plan = Planner::plan(bound).map_err(|e| DbError::PlanError(format!("{:?}", e)))?;

        // 3. Optimize
        let optimizer = Optimizer::new(Arc::clone(&self.accessor), txn);
        let physical = optimizer
            .optimize(plan)
            .map_err(|e| DbError::OptimizerError(format!("{:?}", e)))?;

        // 4. Execute
        let result = executor::execute(physical, self.accessor.as_ref(), txn).await?;

        // Format result
        Ok(format_result(result))
    }

    /// Get the current transaction's Txn struct, or begin an auto-commit transaction.
    fn get_or_begin_txn(&mut self) -> Txn {
        match &self.current_txn {
            Some(t) => Txn {
                id: t.txn_id,
                isolation: convert_isolation(t.isolation_level),
            },
            None => {
                // Auto-commit mode: begin implicit transaction
                let isolation = self
                    .next_txn_isolation_level
                    .take()
                    .unwrap_or(self.session_isolation_level);
                let t = self.txn_manager.begin(isolation);
                let txn = Txn {
                    id: t.txn_id,
                    isolation: convert_isolation(t.isolation_level),
                };
                // For auto-commit, we start the transaction but don't store it
                // (it will auto-commit after the statement)
                txn
            }
        }
    }
}

/// Convert transaction isolation level to common Txn isolation level.
fn convert_isolation(level: IsolationLevel) -> crate::common::txn::IsolationLevel {
    match level {
        IsolationLevel::ReadUncommitted => crate::common::txn::IsolationLevel::ReadUncommitted,
        IsolationLevel::ReadCommitted => crate::common::txn::IsolationLevel::ReadCommitted,
        IsolationLevel::RepeatableRead => crate::common::txn::IsolationLevel::Snapshot,
        IsolationLevel::Snapshot => crate::common::txn::IsolationLevel::Snapshot,
        IsolationLevel::Serializable => crate::common::txn::IsolationLevel::Snapshot,
    }
}

/// Format execution result as a string.
fn format_result(result: ExecutionResult) -> String {
    match result {
        ExecutionResult::Ok(msg) => msg,
        ExecutionResult::RowsAffected(n) => format!("{} row(s) affected", n),
        ExecutionResult::Rows { columns, rows } => {
            if rows.is_empty() {
                return "(0 rows)".to_string();
            }

            let mut output = String::new();

            // Header
            output.push_str(&columns.join(" | "));
            output.push('\n');

            // Separator
            let sep: Vec<String> = columns.iter().map(|c| "-".repeat(c.len().max(4))).collect();
            output.push_str(&sep.join("-+-"));
            output.push('\n');

            // Rows
            for row in &rows {
                let vals: Vec<String> = row.values.iter().map(|v| v.to_string()).collect();
                output.push_str(&vals.join(" | "));
                output.push('\n');
            }

            output.push_str(&format!("({} row(s))", rows.len()));
            output
        }
    }
}
