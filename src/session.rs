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
    pub next_txn_isolation_level: Option<IsolationLevel>,

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
            next_txn_isolation_level: None,
            accessor,
            txn_manager,
            oid_allocator,
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
                        self.current_txn = Some(
                            self.txn_manager
                                .begin(isolation_level.unwrap_or(self.session_isolation_level)),
                        );
                        Ok("BEGIN".into())
                    }
                }
                Statement::Commit => {
                    let current_txn = self.current_txn.take().ok_or(DbError::NotInTransaction)?;

                    if self.txn_manager.get_txn_state(current_txn.id)
                        == crate::transaction::TxnState::Aborted
                    {
                        Ok("ROLLBACK".into())
                    } else {
                        self.txn_manager.commit(&current_txn)?;
                        Ok("COMMIT".into())
                    }
                }
                Statement::Rollback => {
                    let current_txn = self.current_txn.take().ok_or(DbError::NotInTransaction)?;
                    let _ = self.txn_manager.rollback(&current_txn);
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
            let is_auto_commit = self.current_txn.is_none();
            let txn = self.get_or_begin_txn();

            if !is_auto_commit
                && self.txn_manager.get_txn_state(txn.id) == crate::transaction::TxnState::Aborted
            {
                return Err(DbError::InvalidTransactionState(
                    "current transaction is aborted, commands ignored until end of transaction block".into()
                ));
            }

            let result = async {
                let binder = Binder::new(
                    Arc::clone(&self.accessor),
                    Arc::clone(&self.oid_allocator),
                    txn.clone(),
                );
                let mut bound = binder.bind(stmt)?;
                executor::materialize_subqueries(
                    &mut bound,
                    Arc::clone(&self.accessor),
                    txn.clone(),
                )
                .await?;
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
                    }
                    if matches!(
                        exec_result,
                        ExecutionResult::RowsAffected(_) | ExecutionResult::Ok(_)
                    ) {
                        self.accessor
                            .flush()
                            .await
                            .map_err(|e| DbError::Internal(format!("flush failed: {:?}", e)))?;
                    }
                    Ok(format_result(exec_result))
                }
                Err(e) => {
                    let _ = self.txn_manager.rollback(&txn);
                    Err(e)
                }
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

fn format_result(result: ExecutionResult) -> String {
    match result {
        ExecutionResult::Ok(msg) => msg,
        ExecutionResult::RowsAffected(n) => format!("{} row(s) affected", n),
        ExecutionResult::Rows { columns, rows } => {
            if columns.is_empty() {
                return format!("({} row(s))", rows.len());
            }

            let mut col_widths: Vec<usize> = columns.iter().map(|c| c.len()).collect();

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

            let header_cells: Vec<String> = columns
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{:<width$}", c, width = col_widths[i]))
                .collect();
            output.push_str(&header_cells.join(" | "));
            output.push('\n');

            let sep_cells: Vec<String> = col_widths.iter().map(|&w| "-".repeat(w)).collect();
            output.push_str(&sep_cells.join("-+-"));
            output.push('\n');

            for str_row in stringified_rows {
                let row_cells: Vec<String> = str_row
                    .into_iter()
                    .enumerate()
                    .map(|(i, val)| {
                        let width = col_widths.get(i).copied().unwrap_or(0);
                        format!("{:<width$}", val, width = width)
                    })
                    .collect();
                output.push_str(&row_cells.join(" | "));
                output.push('\n');
            }

            output.push_str(&format!("({} row(s))", rows.len()));
            output
        }
    }
}
