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

    pub fn execute_sql<'a>(
        &'a mut self,
        sql: &'a str,
    ) -> impl std::future::Future<Output = Result<String>> + 'a {
        async move {
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
                        self.current_txn = Some(self.txn_manager.begin(isolation_level.unwrap_or(self.session_isolation_level)));
                        Ok("BEGIN".into())
                    }
                }
                Statement::Commit => {
                    let current_txn =
                        self.current_txn.as_mut().ok_or(DbError::NotInTransaction)?;
                    self.txn_manager.commit(current_txn)?;
                    self.current_txn = None;
                    Ok("COMMIT".into())
                }
                Statement::Rollback => {
                    let current_txn =
                        self.current_txn.as_mut().ok_or(DbError::NotInTransaction)?;
                    self.txn_manager.rollback(current_txn)?;
                    self.current_txn = None;
                    Ok("ROLLBACK".into())
                }
                other => self.execute_sql_in_txn(other).await,
            }
        }
    }

    fn execute_sql_in_txn(
        &mut self,
        stmt: Statement,
    ) -> impl std::future::Future<Output = Result<String>> + '_ {
        async move {
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
            let plan =
                Planner::plan(bound).map_err(|e| DbError::PlanError(format!("{:?}", e)))?;

            // 3. Optimize
            let optimizer = Optimizer::new(Arc::clone(&self.accessor), txn);
            let physical = optimizer
                .optimize(plan)
                .map_err(|e| DbError::OptimizerError(format!("{:?}", e)))?;

            // 4. Execute
            let result = executor::execute(physical, self.accessor.as_ref(), txn).await?;

            // 5. Flush on any mutation so state survives a kill before WAL exists.
            if matches!(
                result,
                ExecutionResult::RowsAffected(_) | ExecutionResult::Ok(_)
            ) {
                self.accessor
                    .flush()
                    .await
                    .map_err(|e| DbError::Internal(format!("flush failed: {:?}", e)))?;
            }

            // Format result
            Ok(format_result(result))
        }
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
            if columns.is_empty() {
                return format!("({} row(s))", rows.len());
            }

            // 1. Initialize column widths with the length of the column headers
            let mut col_widths: Vec<usize> = columns.iter().map(|c| c.len()).collect();
            
            // 2. Stringify row values and update maximum column widths
            let mut stringified_rows = Vec::with_capacity(rows.len());
            for row in &rows {
                let str_row: Vec<String> = row.values.iter().map(|v| v.to_string()).collect();
                for (i, val) in str_row.iter().enumerate() {
                    if i < col_widths.len() {
                        col_widths[i] = col_widths[i].max(val.len());
                    }
                }
                stringified_rows.push(str_row);
            }

            let mut output = String::new();

            // 3. Format Header
            let header_cells: Vec<String> = columns
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{:<width$}", c, width = col_widths[i]))
                .collect();
            output.push_str(&header_cells.join(" | "));
            output.push('\n');

            // 4. Format Separator
            let sep_cells: Vec<String> = col_widths
                .iter()
                .map(|&w| "-".repeat(w))
                .collect();
            output.push_str(&sep_cells.join("-+-"));
            output.push('\n');

            // 5. Format Rows
            for str_row in stringified_rows {
                let row_cells: Vec<String> = str_row
                    .into_iter()
                    .enumerate()
                    .map(|(i, val)| {
                        // Safely fallback to 0 width if row length exceeds header length
                        let width = col_widths.get(i).copied().unwrap_or(0);
                        format!("{:<width$}", val, width = width)
                    })
                    .collect();
                output.push_str(&row_cells.join(" | "));
                output.push('\n');
            }

            // 6. Footer
            output.push_str(&format!("({} row(s))", rows.len()));
            output
        }
    }
}