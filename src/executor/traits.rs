use super::error::ExecutorError;
use async_trait::async_trait;
use futures::stream::BoxStream;
use std::sync::Arc;

// Import common types (these will come from your common crate)
use crate::common::{
    ColumnId, DataChunk, IndexId, IsolationLevel, RowHandler, ScalarValue, ScanOptions, TableId,
    TableSchema, TableStatistics, TransactionContext, TransactionId,
};

/// Type alias for a boxed executor that produces DataChunks
/// This is how operators connect to each other in the executor layer
pub type BoxedExecutor = BoxStream<'static, Result<DataChunk, ExecutorError>>;

/// The main storage interface - entry point for all storage operations
/// Storage layer implements this, executor layer uses it
#[async_trait]
pub trait Storage: Send + Sync {
    /// Create a new table with the given schema
    async fn create_table(&self, schema: TableSchema) -> Result<TableId, ExecutorError>;

    /// Drop a table by its ID
    async fn drop_table(&self, table_id: TableId) -> Result<(), ExecutorError>;

    /// Get a table handle by its ID
    async fn get_table(&self, table_id: TableId) -> Result<Arc<dyn Table>, ExecutorError>;

    /// Get a table handle by its name
    async fn get_table_by_name(&self, name: &str) -> Result<Arc<dyn Table>, ExecutorError>;

    /// List all tables in the database
    async fn list_tables(&self) -> Result<Vec<TableId>, ExecutorError>;

    /// Create an index on a table
    async fn create_index(
        &self,
        table_id: TableId,
        index_name: String,
        columns: Vec<ColumnId>,
    ) -> Result<IndexId, ExecutorError>;

    /// Drop an index
    async fn drop_index(&self, index_id: IndexId) -> Result<(), ExecutorError>;
}

/// Represents a single table and provides access to its schema and transactions
/// Storage layer implements this, executor layer uses it
#[async_trait]
pub trait Table: Send + Sync {
    /// Get the table's unique identifier
    fn table_id(&self) -> TableId;

    /// Get the table's schema
    fn schema(&self) -> &TableSchema;

    /// Get the primary key columns
    fn primary_key(&self) -> &[ColumnId];

    /// Get statistics for query planning
    async fn statistics(&self) -> Result<TableStatistics, ExecutorError>;

    /// Begin a new read-write transaction
    async fn begin_transaction(
        &self,
        isolation_level: IsolationLevel,
    ) -> Result<Arc<dyn Transaction>, ExecutorError>;

    /// Begin a new read-only transaction (may be optimized by storage)
    async fn begin_read_transaction(
        &self,
        isolation_level: IsolationLevel,
    ) -> Result<Arc<dyn Transaction>, ExecutorError>;

    /// Get all indices on this table
    async fn list_indices(&self) -> Result<Vec<IndexId>, ExecutorError>;
}

/// Represents an ACID transaction boundary
/// Storage layer implements this, executor layer uses it
/// This is the core trait that gets passed through your operator tree
#[async_trait]
pub trait Transaction: Send + Sync {
    /// Get the transaction context
    fn context(&self) -> &TransactionContext;

    /// Scan the table and return an iterator
    /// Used by TableScanExecutor
    async fn scan(&self, options: ScanOptions) -> Result<Box<dyn TxnIterator>, ExecutorError>;

    /// Scan using an index
    /// Used by IndexScanExecutor
    async fn index_scan(
        &self,
        index_id: IndexId,
        options: ScanOptions,
    ) -> Result<Box<dyn TxnIterator>, ExecutorError>;

    /// Get a single row by primary key (point lookup)
    /// Optimized path for WHERE primary_key = value queries
    async fn get_by_key(&self, key: Vec<ScalarValue>) -> Result<Option<DataChunk>, ExecutorError>;

    /// Append new rows to the table
    /// Used by InsertExecutor
    /// Returns RowHandlers for the inserted rows
    async fn append(&self, chunk: DataChunk) -> Result<Vec<RowHandler>, ExecutorError>;

    /// Update specific rows identified by their handlers
    /// Used by UpdateExecutor
    /// Returns the number of rows actually updated
    async fn update(
        &self,
        handlers: Vec<RowHandler>,
        chunk: DataChunk,
    ) -> Result<usize, ExecutorError>;

    /// Delete specific rows identified by their handlers
    /// Used by DeleteExecutor
    /// Returns the number of rows actually deleted
    async fn delete(&self, handlers: Vec<RowHandler>) -> Result<usize, ExecutorError>;

    /// Commit the transaction
    /// Consumes the transaction (Box<Self>) to ensure it can't be used after commit
    async fn commit(self: Box<Self>) -> Result<(), ExecutorError>;

    /// Abort the transaction
    /// Consumes the transaction (Box<Self>) to ensure it can't be used after abort
    async fn abort(self: Box<Self>) -> Result<(), ExecutorError>;
}

/// Iterator for scanning table data within a transaction
#[async_trait]
pub trait TxnIterator: Send {
    /// Get the next batch of rows
    /// Returns None when no more data is available
    async fn next_batch(&mut self) -> Result<Option<DataChunk>, ExecutorError>;

    /// Get the schema of the data being returned
    fn schema(&self) -> &TableSchema;

    /// Hint to the iterator about how many more batches we expect to consume
    /// Storage can use this for prefetching optimizations
    fn set_batch_hint(&mut self, _hint: usize) {
        // Default implementation does nothing
    }
}

/// Provides statistics for query optimization
/// Storage layer implements this, optimizer/planner uses it
#[async_trait]
pub trait StatisticsProvider: Send + Sync {
    /// Get statistics for a table
    async fn get_table_statistics(
        &self,
        table_id: TableId,
    ) -> Result<TableStatistics, ExecutorError>;

    /// Get statistics for an index
    async fn get_index_statistics(
        &self,
        index_id: IndexId,
    ) -> Result<TableStatistics, ExecutorError>;

    /// Refresh statistics (e.g., after bulk insert)
    async fn refresh_statistics(&self, table_id: TableId) -> Result<(), ExecutorError>;
}

/// Helper trait for operators that need to be initialized before execution
/// This is internal to the executor layer
#[async_trait]
pub trait Executor: Send {
    /// Initialize the executor (e.g., open files, prepare buffers)
    async fn open(&mut self) -> Result<(), ExecutorError> {
        Ok(())
    }

    /// Execute and produce the next chunk
    /// This is called repeatedly until it returns None
    async fn next(&mut self) -> Result<Option<DataChunk>, ExecutorError>;

    /// Close and cleanup resources
    async fn close(&mut self) -> Result<(), ExecutorError> {
        Ok(())
    }

    /// Get the output schema of this executor
    fn schema(&self) -> &TableSchema;
}

/// Convert an Executor into a Stream (BoxedExecutor)
/// This allows operators to be composed in a pipeline
pub fn into_stream(mut executor: impl Executor + 'static) -> BoxedExecutor {
    Box::pin(async_stream::try_stream! {
        executor.open().await?;

        while let Some(chunk) = executor.next().await? {
            yield chunk;
        }

        executor.close().await?;
    })
}
