use futures::Stream;

use crate::{
    buffer, catalog,
    common::{aliases, txn::Txn},
};

pub enum Error {
    BufferError(buffer::Error),
    TupleNonExistent,
    /// Operation Txn, Tuple XMIN, Tuple XMAX
    TupleNotVisible(aliases::TxnId, aliases::TxnId, aliases::TxnId),
}

pub type Result<T> = std::result::Result<T, Error>;

// Probably try to remove the associated types later
pub trait Accessor: Send + Sync + 'static {
    fn table_scan(
        &self,
        txn: Txn,
        table_oid: aliases::OId,
    ) -> impl Future<Output = Result<impl Stream<Item = (Vec<u8>, aliases::RecordId)> + Send>> + '_ + Send;
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
    ) -> impl Future<Output = Result<impl Stream<Item = (Vec<u8>, aliases::RecordId)> + Send>> + Send;
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
}
