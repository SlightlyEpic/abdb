use std::sync::{Arc, RwLock};

use futures::Stream;

use crate::{
    buffer::BufferPool,
    catalog,
    common::{aliases, txn::Txn},
};

use super::{
    accessor::{Accessor, Error, Result},
    btree,
    catalog_cache::CatalogCache,
    heap,
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

    /// Flush all dirty pages (and storage metadata) to disk.
    pub async fn flush(&self) -> Result<()> {
        self.bp.flush_all_dirty().await.map_err(Error::BufferError)
    }

    /// Register a user-created table in the catalog cache.
    ///
    /// Returns an error if a table with the same OID is already registered.
    pub fn register_table(
        &self,
        table: catalog::Table,
        columns: Vec<catalog::Column>,
    ) -> Result<()> {
        let mut cache = self.catalog.write().expect("catalog lock poisoned");
        let oid = table.oid;
        cache.register_table(table)?;
        cache.register_columns(oid, columns);
        Ok(())
    }

    /// Register a user-created index in the catalog cache.
    ///
    /// Returns an error if an index with the same OID is already registered.
    pub fn register_index(&self, index: catalog::Index) -> Result<()> {
        let mut cache = self.catalog.write().expect("catalog lock poisoned");
        cache.register_index(index)
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
    ) -> impl Future<
        Output = Result<impl Stream<Item = Result<(Vec<u8>, aliases::RecordId)>> + Send>,
    >
    + '_
    + Send {
        async move {
            let file_id = self.table_file_id(table_oid)?;
            heap::scan(&*self.bp, file_id, txn).await
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
    ) -> impl Future<
        Output = Result<impl Stream<Item = Result<(Vec<u8>, aliases::RecordId)>> + Send>,
    >
    + '_
    + Send {
        async move {
            let file_id = self.index_file_id(index_oid)?;
            btree::scan(&*self.bp, file_id, txn, start_key, end_key).await
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

    fn catalog_get_table_by_name(&self, _txn: Txn, table_name: &str) -> Result<catalog::Table> {
        let cache = self.catalog.read().expect("catalog lock poisoned");
        cache.get_table_by_name(table_name)
    }

    fn catalog_get_table_by_oid(
        &self,
        _txn: Txn,
        table_oid: aliases::OId,
    ) -> Result<catalog::Table> {
        let cache = self.catalog.read().expect("catalog lock poisoned");
        cache.get_table_by_oid(table_oid)
    }

    fn catalog_get_index_by_name(&self, _txn: Txn, index_name: &str) -> Result<catalog::Index> {
        let cache = self.catalog.read().expect("catalog lock poisoned");
        cache.get_index_by_name(index_name)
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

    // -- DDL operations --------------------------------------------------------

    fn create_table(
        &self,
        txn: Txn,
        table: catalog::Table,
        columns: Vec<catalog::Column>,
    ) -> impl Future<Output = Result<()>> + '_ + Send {
        async move {
            use crate::buffer::PageWriteGuard;
            use crate::common::constants::PAGE_BUF_SIZE;
            use crate::page::overlays::file_header::HeapFileHeaderPage;

            let file_id = table.file_id;
            let table_oid = table.oid;

            // 1. Initialize the heap file header at offset 0
            let header_loc = aliases::PPageId {
                file: file_id,
                offset: 0,
            };

            // Allocate LPageId, register directory mapping, zero-extend file.
            let header_lpage_id = self
                .bp
                .init_page_at_loc(header_loc)
                .await
                .map_err(Error::BufferError)?;

            let mut guard = self
                .bp
                .fetch_page_at_loc_write(header_loc)
                .await
                .map_err(Error::BufferError)?;

            let buffer = &mut *guard;
            buffer.fill(0);

            let temp_buffer = HeapFileHeaderPage::init(
                [0u8; PAGE_BUF_SIZE],
                header_lpage_id,
                table_oid,
            );
            buffer.copy_from_slice(temp_buffer.as_buffer());

            guard.mark_dirty().map_err(Error::BufferError)?;
            drop(guard);

            // 2. Register in catalog cache
            self.register_table(table, columns)?;

            // 3. Persist to system tables
            // Extract data from cache before any awaits (RwLockGuard is not Send)
            let (table_name, columns_data): (String, Vec<(u32, u32, String, u8, u16, bool)>) = {
                let cache = self.catalog.read().expect("catalog lock poisoned");
                let table_info = cache.get_table_by_oid(table_oid)?;
                let columns = cache.get_table_columns(table_oid)?;
                let cols_data = columns
                    .iter()
                    .map(|c| {
                        (
                            c.oid,
                            c.table_oid,
                            c.name.to_string(),
                            c.type_id as u8,
                            c.position,
                            c.nullable,
                        )
                    })
                    .collect();
                (table_info.name.to_string(), cols_data)
            };

            // Insert into sys_tables
            let sys_tables_layout =
                crate::databox::TupleLayout::from(catalog::schema::SYS_COLUMNS_TABLES_TABLE.to_vec());

            let sys_tables_cols = ["oid", "name", "file_id"];
            let sys_tables_vals = [
                crate::databox::Value::U32(table_oid),
                crate::databox::Value::String(table_name),
                crate::databox::Value::U32(file_id),
            ];

            let tuple_bytes = sys_tables_layout.encode_tuple(&sys_tables_cols, &sys_tables_vals);

            heap::insert(
                &*self.bp,
                crate::common::constants::SYS_TABLE_TABLES_FID,
                &txn,
                &tuple_bytes,
            )
            .await?;

            // Insert columns into sys_columns
            let sys_columns_layout =
                crate::databox::TupleLayout::from(catalog::schema::SYS_COLUMNS_COLUMNS_TABLE.to_vec());

            let sys_columns_cols = ["oid", "table_oid", "name", "type_id", "position", "nullable"];

            for (col_oid, col_table_oid, col_name, col_type, col_pos, col_null) in columns_data {
                let col_vals = [
                    crate::databox::Value::U32(col_oid),
                    crate::databox::Value::U32(col_table_oid),
                    crate::databox::Value::String(col_name),
                    crate::databox::Value::U8(col_type),
                    crate::databox::Value::U16(col_pos),
                    crate::databox::Value::Bool(col_null),
                ];

                let col_bytes = sys_columns_layout.encode_tuple(&sys_columns_cols, &col_vals);

                heap::insert(
                    &*self.bp,
                    crate::common::constants::SYS_TABLE_COLUMNS_FID,
                    &txn,
                    &col_bytes,
                )
                .await?;
            }

            // Persist pages + page directory so restart can see the new table.
            self.bp
                .flush_all_dirty()
                .await
                .map_err(Error::BufferError)?;

            Ok(())
        }
    }

    fn drop_table(
        &self,
        txn: Txn,
        table_oid: aliases::OId,
        table_name: String,
    ) -> impl Future<Output = Result<()>> + '_ + Send {
        async move {
            {
                let mut cache = self.catalog.write().expect("catalog lock poisoned");
                cache.drop_table(&table_name, table_oid);
            }

            use futures::StreamExt;

            let sys_tables_fid = crate::common::constants::SYS_TABLE_TABLES_FID;
            let tables_layout = crate::databox::TupleLayout::from(catalog::schema::SYS_COLUMNS_TABLES_TABLE.to_vec());
            let mut tables_stream = std::pin::pin!(heap::scan(&*self.bp, sys_tables_fid, txn).await?);

            while let Some(result) = tables_stream.next().await {
                let (tuple_bytes, rid) = result?;
                if let Some(oid) = tables_layout.read_field("oid", 0, &tuple_bytes).and_then(|v| v.as_u32()) {
                    if oid == table_oid {
                        heap::delete(&*self.bp, sys_tables_fid, &txn, rid).await?;
                        break;
                    }
                }
            }

            let sys_columns_fid = crate::common::constants::SYS_TABLE_COLUMNS_FID;
            let columns_layout = crate::databox::TupleLayout::from(catalog::schema::SYS_COLUMNS_COLUMNS_TABLE.to_vec());
            let mut columns_stream = std::pin::pin!(heap::scan(&*self.bp, sys_columns_fid, txn).await?);

            while let Some(result) = columns_stream.next().await {
                let (tuple_bytes, rid) = result?;
                if let Some(t_oid) = columns_layout.read_field("table_oid", 1, &tuple_bytes).and_then(|v| v.as_u32()) {
                    if t_oid == table_oid {
                        heap::delete(&*self.bp, sys_columns_fid, &txn, rid).await?;
                    }
                }
            }

            self.bp.flush_all_dirty().await.map_err(Error::BufferError)?;

            Ok(())
        }
    }
}
