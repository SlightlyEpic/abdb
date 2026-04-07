use futures::Stream;

use crate::{
    buffer, catalog, databox,
    common::{aliases, txn::Txn},
};

#[derive(Debug)]
pub enum Error {
    BufferError(buffer::Error),
    TupleNonExistent,
    /// Tuple was already deleted by another transaction.
    AlreadyDeleted(aliases::TxnId),
    /// Operation Txn, Tuple XMIN, Tuple XMAX
    TupleNotVisible(aliases::TxnId, aliases::TxnId, aliases::TxnId),
    NotFound(String),
    PageCorruption(String),
    /// Duplicate OID registration in catalog cache.
    DuplicateOId(aliases::OId),
    /// File capacity exceeded (e.g. num_pages overflow).
    CapacityExceeded(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub trait Accessor: Send + Sync {
    fn table_scan(
        &self,
        txn: Txn,
        table_oid: aliases::OId,
    ) -> impl Future<
        Output = Result<impl Stream<Item = Result<(Vec<u8>, aliases::RecordId)>> + Send>,
    >
    + '_
    + Send;
    fn table_insert(
        &self,
        txn: Txn,
        table_oid: aliases::OId,
        tuple: Vec<u8>,
    ) -> impl Future<Output = Result<aliases::RecordId>> + '_ + Send;
    fn table_get(
        &self,
        txn: Txn,
        table_oid: aliases::OId,
        rid: aliases::RecordId,
    ) -> impl Future<Output = Result<Vec<u8>>> + '_ + Send;
    fn table_delete(
        &self,
        txn: Txn,
        table_oid: aliases::OId,
        rid: aliases::RecordId,
    ) -> impl Future<Output = Result<()>> + '_ + Send;

    fn index_scan(
        &self,
        txn: Txn,
        index_oid: aliases::OId,
        start_key: Option<Vec<u8>>,
        end_key: Option<Vec<u8>>,
    ) -> impl Future<
        Output = Result<impl Stream<Item = Result<(Vec<u8>, aliases::RecordId)>> + Send>,
    >
    + '_
    + Send;
    fn index_insert(
        &self,
        txn: Txn,
        index_oid: aliases::OId,
        key: Vec<u8>,
        rid: aliases::RecordId,
    ) -> impl Future<Output = Result<()>> + '_ + Send;
    /// Only usable for unique indexes
    fn index_get(
        &self,
        txn: Txn,
        index_oid: aliases::OId,
        key: Vec<u8>,
    ) -> impl Future<Output = Result<aliases::RecordId>> + '_ + Send;
    fn index_delete(
        &self,
        txn: Txn,
        index_oid: aliases::OId,
        key: Vec<u8>,
        rid: aliases::RecordId,
    ) -> impl Future<Output = Result<()>> + '_ + Send;

    // The accessor will ensure that all catalog pages are always held in memory
    // So catalog operations should essentially be O(1)

    fn catalog_get_table_by_name(&self, txn: Txn, table_name: String) -> Result<catalog::Table>;
    fn catalog_get_table_by_oid(&self, txn: Txn, table_oid: aliases::OId)
    -> Result<catalog::Table>;
    fn catalog_get_index_by_name(&self, txn: Txn, index_name: String) -> Result<catalog::Index>;
    fn catalog_get_index_by_oid(&self, txn: Txn, index_oid: aliases::OId)
    -> Result<catalog::Index>;
    fn catalog_get_table_columns(
        &self,
        txn: Txn,
        table_oid: aliases::OId,
    ) -> Result<Vec<catalog::Column>>;

    // -- DDL ----------------------------------------------------------------

    /// Definition of a single column for `create_table`.
    /// Mirrored as a free type below for ergonomics.

    /// Create a new table: allocates oids/file id, creates the heap file on
    /// disk, initializes its header page, registers it in the catalog cache,
    /// and persists rows into `sys_tables` and `sys_columns` so the table
    /// survives a restart (assuming a future bootstrap loader).
    fn create_table(
        &self,
        txn: Txn,
        name: String,
        columns: Vec<NewColumn>,
    ) -> impl Future<Output = Result<catalog::Table>> + '_ + Send;

    /// Drop a table: refuses if any registered index targets it, deletes
    /// the catalog rows from sys_tables/sys_columns, deregisters the cache
    /// entry, and removes the heap file from disk.
    fn drop_table(
        &self,
        txn: Txn,
        table_oid: aliases::OId,
    ) -> impl Future<Output = Result<()>> + '_ + Send;

    /// Create an index over (table_oid, column_oid): allocates an oid and
    /// file id, creates the index file on disk, initializes its header page,
    /// registers in the cache, and persists a row into `sys_indexes`.
    fn create_index(
        &self,
        txn: Txn,
        name: String,
        table_oid: aliases::OId,
        column_oid: aliases::OId,
    ) -> impl Future<Output = Result<catalog::Index>> + '_ + Send;

    /// Drop an index: deletes its row from sys_indexes, deregisters the
    /// cache entry, and removes the index file from disk.
    fn drop_index(
        &self,
        txn: Txn,
        index_oid: aliases::OId,
    ) -> impl Future<Output = Result<()>> + '_ + Send;
}

/// Column specification for `create_table`.
#[derive(Clone, Debug)]
pub struct NewColumn {
    pub name: String,
    pub type_id: databox::DataType,
    pub nullable: bool,
}
