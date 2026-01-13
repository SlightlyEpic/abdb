use crate::{
    common::{aliases, txn::Txn},
    storage::{
        disk,
        objects::{index, table},
    },
};

pub enum StorageError {
    DiskError(disk::DiskError),
}

/// Create methods on Storage are responsible for modifying the catalog
/// as well as performing initialization such as creating the files
/// and setting up the file header
pub trait Storage: Send + Sync + 'static {
    type Table: table::Table;
    type Index: index::Index;

    fn get_table(
        &self,
        txn: Txn,
        table_oid: aliases::OId,
    ) -> impl Future<Output = Result<Self::Table, StorageError>> + '_ + Send;
    fn create_table(
        &self,
        txn: Txn,
        // table: &<Self::Catalog as catalog::Catalog>::TableDef,
        // columns: &<Self::Catalog as catalog::Catalog>::ColumnDef,
    ) -> impl Future<Output = Result<(), StorageError>> + '_ + Send;
    fn drop_table(
        &self,
        txn: Txn,
        table_oid: aliases::OId,
    ) -> impl Future<Output = Result<(), StorageError>> + '_ + Send;

    fn get_index(
        &self,
        txn: Txn,
        index_oid: aliases::OId,
    ) -> impl Future<Output = Result<Self::Index, StorageError>> + '_ + Send;
    fn create_index(
        &self,
        txn: Txn,
        index_name: String,
        // schema: &<Self::Catalog as catalog::Catalog>::IndexDef,
    ) -> impl Future<Output = Result<(), StorageError>> + '_ + Send;
    fn drop_index(
        &self,
        txn: Txn,
        index_name: String,
    ) -> impl Future<Output = Result<(), StorageError>> + '_ + Send;
}
