use std::sync::{Arc, RwLock};

use futures::Stream;

use crate::{
    buffer::BufferPool,
    catalog,
    common::{aliases, txn::Txn},
};

use super::{
    accessor::{Accessor, Result},
    btree, catalog_cache::CatalogCache, heap,
};

/// Concrete implementation of the Accessor trait.
///
/// Bridges the BufferPool (page-level I/O) with the overlay layer (page
/// structure) to provide tuple-level operations to the executor.
///
/// # Type Parameters
///
/// * `B` — the BufferPool implementation used for page I/O and latching.
///
/// # Thread Safety
///
/// `AccessorImpl` is `Send + Sync + 'static`. The catalog cache uses a
/// `RwLock` for concurrent read access with exclusive write access during DDL.
pub struct AccessorImpl<B: BufferPool> {
    bp: Arc<B>,
    catalog: RwLock<CatalogCache>,
}

impl<B: BufferPool> AccessorImpl<B> {
    /// Create a new AccessorImpl with the given buffer pool.
    ///
    /// Initializes the catalog cache with system table definitions.
    /// In a production system, this would also scan the system tables
    /// to load user-created tables and indexes.
    pub fn new(bp: Arc<B>) -> Self {
        Self {
            bp,
            catalog: RwLock::new(CatalogCache::new()),
        }
    }

    /// Register a user-created table in the catalog cache.
    pub fn register_table(
        &self,
        table: catalog::Table,
        columns: Vec<catalog::Column>,
    ) {
        let mut cache = self.catalog.write().expect("catalog lock poisoned");
        let oid = table.oid;
        cache.register_table(table);
        cache.register_columns(oid, columns);
    }

    /// Register a user-created index in the catalog cache.
    pub fn register_index(&self, index: catalog::Index) {
        let mut cache = self.catalog.write().expect("catalog lock poisoned");
        cache.register_index(index);
    }

    /// Look up a table's file_id from the catalog cache.
    fn table_file_id(&self, table_oid: aliases::OId) -> Result<aliases::FileId> {
        let cache = self.catalog.read().expect("catalog lock poisoned");
        let table = cache.get_table_by_oid(table_oid)?;
        Ok(table.file_id)
    }

    /// Look up an index's file_id from the catalog cache.
    fn index_file_id(&self, index_oid: aliases::OId) -> Result<aliases::FileId> {
        let cache = self.catalog.read().expect("catalog lock poisoned");
        let index = cache.get_index_by_oid(index_oid)?;
        Ok(index.file_id)
    }
}

// ============================================================================
// ACCESSOR TRAIT IMPLEMENTATION
// ============================================================================

impl<B: BufferPool> Accessor for AccessorImpl<B> {
    // -- Table operations ----------------------------------------------------

    fn table_scan(
        &self,
        txn: Txn,
        table_oid: aliases::OId,
    ) -> impl Future<Output = Result<impl Stream<Item = (Vec<u8>, aliases::RecordId)> + Send>> + '_ + Send
    {
        async move {
            let file_id = self.table_file_id(table_oid)?;
            heap::scan(Arc::clone(&self.bp), file_id, txn).await
        }
    }

    fn table_insert(
        &self,
        txn: Txn,
        table_oid: aliases::OId,
        tuple: Vec<u8>,
    ) -> impl Future<Output = Result<aliases::RecordId>> + '_ + Send {
        async move {
            let file_id = self.table_file_id(table_oid)?;
            heap::insert(&*self.bp, file_id, &txn, &tuple).await
        }
    }

    fn table_get(
        &self,
        txn: Txn,
        table_oid: aliases::OId,
        rid: aliases::RecordId,
    ) -> impl Future<Output = Result<Vec<u8>>> + '_ + Send {
        async move {
            let file_id = self.table_file_id(table_oid)?;
            heap::get(&*self.bp, file_id, &txn, rid).await
        }
    }

    fn table_delete(
        &self,
        txn: Txn,
        table_oid: aliases::OId,
        rid: aliases::RecordId,
    ) -> impl Future<Output = Result<()>> + '_ + Send {
        async move {
            let file_id = self.table_file_id(table_oid)?;
            heap::delete(&*self.bp, file_id, &txn, rid).await
        }
    }

    // -- Index operations ----------------------------------------------------

    fn index_scan(
        &self,
        txn: Txn,
        index_oid: aliases::OId,
        start_key: Option<Vec<u8>>,
        end_key: Option<Vec<u8>>,
    ) -> impl Future<Output = Result<impl Stream<Item = (Vec<u8>, aliases::RecordId)> + Send>> + Send
    {
        let bp = Arc::clone(&self.bp);
        let file_id_result = self.index_file_id(index_oid);

        async move {
            let file_id = file_id_result?;
            btree::scan(bp, file_id, txn, start_key, end_key).await
        }
    }

    fn index_insert(
        &self,
        txn: Txn,
        index_oid: aliases::OId,
        key: Vec<u8>,
        rid: aliases::RecordId,
    ) -> impl Future<Output = Result<()>> + '_ + Send {
        async move {
            let _ = txn; // reserved for future MVCC on indexes
            let file_id = self.index_file_id(index_oid)?;
            btree::insert(&*self.bp, file_id, &key, rid).await
        }
    }

    fn index_get(
        &self,
        txn: Txn,
        index_oid: aliases::OId,
        key: Vec<u8>,
    ) -> impl Future<Output = Result<aliases::RecordId>> + '_ + Send {
        async move {
            let _ = txn;
            let file_id = self.index_file_id(index_oid)?;
            btree::get(&*self.bp, file_id, &key).await
        }
    }

    fn index_delete(
        &self,
        txn: Txn,
        index_oid: aliases::OId,
        key: Vec<u8>,
        rid: aliases::RecordId,
    ) -> impl Future<Output = Result<()>> + '_ + Send {
        async move {
            let _ = txn;
            let file_id = self.index_file_id(index_oid)?;
            btree::delete(&*self.bp, file_id, &key, rid).await
        }
    }

    // -- Catalog operations (synchronous, from cache) ------------------------

    fn catalog_get_table_by_name(
        &self,
        _txn: Txn,
        table_name: String,
    ) -> Result<catalog::Table> {
        let cache = self.catalog.read().expect("catalog lock poisoned");
        cache.get_table_by_name(&table_name)
    }

    fn catalog_get_table_by_oid(
        &self,
        _txn: Txn,
        table_oid: aliases::OId,
    ) -> Result<catalog::Table> {
        let cache = self.catalog.read().expect("catalog lock poisoned");
        cache.get_table_by_oid(table_oid)
    }

    fn catalog_get_index_by_name(
        &self,
        _txn: Txn,
        index_name: String,
    ) -> Result<catalog::Index> {
        let cache = self.catalog.read().expect("catalog lock poisoned");
        cache.get_index_by_name(&index_name)
    }

    fn catalog_get_index_by_oid(
        &self,
        _txn: Txn,
        index_oid: aliases::OId,
    ) -> Result<catalog::Index> {
        let cache = self.catalog.read().expect("catalog lock poisoned");
        cache.get_index_by_oid(index_oid)
    }

    fn catalog_get_table_columns(
        &self,
        _txn: Txn,
        table_oid: aliases::OId,
    ) -> Result<Vec<catalog::Column>> {
        let cache = self.catalog.read().expect("catalog lock poisoned");
        cache.get_table_columns(table_oid)
    }
}
