use std::sync::{Arc, RwLock};
use futures::Stream;

use crate::{
    buffer::BufferPool,
    catalog,
    common::{aliases, txn::Txn},
    databox::Value,
};

use super::{
    accessor::{Accessor, Error, Result},
    btree,
    catalog_cache::CatalogCache,
    heap,
};

pub struct AccessorImpl<B: BufferPool> {
    bp: Arc<B>,
    catalog: RwLock<CatalogCache>,
}

impl<B: BufferPool> AccessorImpl<B> {
    pub fn new(bp: Arc<B>) -> Self {
        Self {
            bp,
            catalog: RwLock::new(CatalogCache::new()),
        }
    }

    pub async fn flush(&self) -> Result<()> {
        self.bp.flush_all_dirty().await.map_err(Error::BufferError)
    }

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

    pub fn register_index(&self, index: catalog::Index) -> Result<()> {
        let mut cache = self.catalog.write().expect("catalog lock poisoned");
        cache.register_index(index)
    }

    fn table_file_id(&self, txn: &Txn, table_oid: aliases::OId) -> Result<aliases::FileId> {
        let cache = self.catalog.read().expect("catalog lock poisoned");
        let table = cache.get_table_by_oid(txn, table_oid)?;
        Ok(table.file_id)
    }

    fn index_file_id(&self, txn: &Txn, index_oid: aliases::OId) -> Result<aliases::FileId> {
        let cache = self.catalog.read().expect("catalog lock poisoned");
        let index = cache.get_index_by_oid(txn, index_oid)?;
        Ok(index.file_id)
    }
}

// ============================================================================
// ACCESSOR TRAIT IMPLEMENTATION
// ============================================================================

impl<B: BufferPool> Accessor for AccessorImpl<B> {
    fn table_scan(
        &self,
        txn: Txn,
        table_oid: aliases::OId,
    ) -> impl Future<Output = Result<impl Stream<Item = Result<(Vec<u8>, aliases::RecordId)>> + Send>> + '_ + Send {
        async move {
            let file_id = self.table_file_id(&txn, table_oid)?;
            heap::scan(&*self.bp, file_id, txn.clone()).await
        }
    }

    fn table_insert(
        &self,
        txn: Txn,
        table_oid: aliases::OId,
        tuple: Vec<u8>,
    ) -> impl Future<Output = Result<aliases::RecordId>> + '_ + Send {
        async move {
            let file_id = self.table_file_id(&txn, table_oid)?;
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
            let file_id = self.table_file_id(&txn, table_oid)?;
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
            let file_id = self.table_file_id(&txn, table_oid)?;
            heap::delete(&*self.bp, file_id, &txn, rid).await
        }
    }

    fn index_scan(
        &self,
        txn: Txn,
        index_oid: aliases::OId,
        start_key: Option<Vec<u8>>,
        end_key: Option<Vec<u8>>,
    ) -> impl Future<Output = Result<impl Stream<Item = Result<(Vec<u8>, aliases::RecordId)>> + Send>> + '_ + Send {
        async move {
            let file_id = self.index_file_id(&txn, index_oid)?;
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
            let file_id = self.index_file_id(&txn, index_oid)?;
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
            let file_id = self.index_file_id(&txn, index_oid)?;
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
            let file_id = self.index_file_id(&txn, index_oid)?;
            btree::delete(&*self.bp, file_id, &key, rid).await
        }
    }

    fn catalog_get_table_by_name(&self, txn: Txn, table_name: &str) -> Result<catalog::Table> {
        let cache = self.catalog.read().expect("catalog lock poisoned");
        cache.get_table_by_name(&txn, table_name)
    }

    fn catalog_get_table_by_oid(
        &self,
        txn: Txn,
        table_oid: aliases::OId,
    ) -> Result<catalog::Table> {
        let cache = self.catalog.read().expect("catalog lock poisoned");
        cache.get_table_by_oid(&txn, table_oid)
    }

    fn catalog_get_index_by_name(&self, txn: Txn, index_name: &str) -> Result<catalog::Index> {
        let cache = self.catalog.read().expect("catalog lock poisoned");
        cache.get_index_by_name(&txn, index_name)
    }

    fn catalog_get_index_by_oid(
        &self,
        txn: Txn,
        index_oid: aliases::OId,
    ) -> Result<catalog::Index> {
        let cache = self.catalog.read().expect("catalog lock poisoned");
        cache.get_index_by_oid(&txn, index_oid)
    }

    fn catalog_get_table_columns(
        &self,
        txn: Txn,
        table_oid: aliases::OId,
    ) -> Result<Vec<catalog::Column>> {
        let cache = self.catalog.read().expect("catalog lock poisoned");
        cache.get_table_columns(&txn, table_oid)
    }

    fn catalog_get_all_tables(&self, txn: Txn) -> Result<Vec<catalog::Table>> {
        let cache = self.catalog.read().expect("catalog lock poisoned");
        Ok(cache.get_all_tables(&txn))
    }

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

            let header_loc = aliases::PPageId { file: file_id, offset: 0 };
            let header_lpage_id = self.bp.init_page_at_loc(header_loc).await.map_err(Error::BufferError)?;

            let mut guard = self.bp.fetch_page_at_loc_write(header_loc).await.map_err(Error::BufferError)?;
            let buffer = &mut *guard;
            buffer.fill(0);

            let temp_buffer = HeapFileHeaderPage::init([0u8; PAGE_BUF_SIZE], header_lpage_id, table_oid);
            buffer.copy_from_slice(temp_buffer.as_buffer());
            guard.mark_dirty().map_err(Error::BufferError)?;
            drop(guard);

            // Inject MVCC Tracking Into Cache
            let mut mem_table = table.clone();
            mem_table.xmin = txn.id;
            mem_table.xmax = 0;
            
            let mut mem_cols = columns.clone();
            for c in &mut mem_cols {
                c.xmin = txn.id;
                c.xmax = 0;
            }
            self.register_table(mem_table, mem_cols)?;

            let (table_name, columns_data): (String, Vec<(u32, u32, String, u8, u16, bool, bool, bool, Option<String>)>) = {
                let cache = self.catalog.read().expect("catalog lock poisoned");
                let table_info = cache.get_table_by_oid(&txn, table_oid)?;
                let columns = cache.get_table_columns(&txn, table_oid)?;
                let cols_data = columns
                    .iter()
                    .map(|c| (
                        c.oid, c.table_oid, c.name.to_string(), c.type_id as u8, 
                        c.position, c.nullable, c.is_unique, c.is_primary_key,
                        c.default_val.as_ref().map(|v| v.to_string()),
                    ))
                    .collect();
                (table_info.name.to_string(), cols_data)
            };

            let sys_tables_layout = crate::databox::TupleLayout::from(catalog::schema::SYS_COLUMNS_TABLES_TABLE.to_vec());
            let sys_tables_cols = ["oid", "name", "file_id"];
            let sys_tables_vals = [
                crate::databox::Value::U32(table_oid),
                crate::databox::Value::String(table_name),
                crate::databox::Value::U32(file_id),
            ];
            let tuple_bytes = sys_tables_layout.encode_tuple(&sys_tables_cols, &sys_tables_vals);
            heap::insert(&*self.bp, crate::common::constants::SYS_TABLE_TABLES_FID, &txn, &tuple_bytes).await?;

            let sys_columns_layout = crate::databox::TupleLayout::from(catalog::schema::SYS_COLUMNS_COLUMNS_TABLE.to_vec());
            let sys_columns_cols = ["oid", "table_oid", "name", "type_id", "position", "nullable", "is_unique", "is_primary_key", "default_val"];
            for (col_oid, col_table_oid, col_name, col_type, col_pos, col_null, col_uniq, col_pk, col_def_val) in columns_data {
                let col_vals = [
                    crate::databox::Value::U32(col_oid),
                    crate::databox::Value::U32(col_table_oid),
                    crate::databox::Value::String(col_name),
                    crate::databox::Value::U8(col_type),
                    crate::databox::Value::U16(col_pos),
                    crate::databox::Value::Bool(col_null),
                    crate::databox::Value::Bool(col_uniq),
                    crate::databox::Value::Bool(col_pk),
                    col_def_val.map(crate::databox::Value::String).unwrap_or(crate::databox::Value::Null),
                ];
                let col_bytes = sys_columns_layout.encode_tuple(&sys_columns_cols, &col_vals);
                heap::insert(&*self.bp, crate::common::constants::SYS_TABLE_COLUMNS_FID, &txn, &col_bytes).await?;
            }

            self.bp.flush_all_dirty().await.map_err(Error::BufferError)?;
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
                cache.drop_table(&table_name, table_oid, &txn);
            }

            use futures::StreamExt;

            let sys_tables_fid = crate::common::constants::SYS_TABLE_TABLES_FID;
            let tables_layout = crate::databox::TupleLayout::from(catalog::schema::SYS_COLUMNS_TABLES_TABLE.to_vec());
            let mut tables_stream = std::pin::pin!(heap::scan(&*self.bp, sys_tables_fid, txn.clone()).await?);

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
            let mut columns_stream = std::pin::pin!(heap::scan(&*self.bp, sys_columns_fid, txn.clone()).await?);

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

    fn add_column(
        &self,
        txn: Txn,
        table_oid: aliases::OId,
        column: catalog::Column,
    ) -> impl Future<Output = Result<()>> + '_ + Send {
        async move {
            {
                let mut cache = self.catalog.write().expect("catalog lock poisoned");
                cache.add_column(table_oid, column.clone(), &txn)?;
            }

            let sys_columns_layout = crate::databox::TupleLayout::from(catalog::schema::SYS_COLUMNS_COLUMNS_TABLE.to_vec());
            let sys_columns_cols = ["oid", "table_oid", "name", "type_id", "position", "nullable", "is_unique", "is_primary_key", "default_val"];
            let col_vals = [
                Value::U32(column.oid),
                Value::U32(column.table_oid),
                Value::String(column.name.to_string()),
                Value::U8(column.type_id as u8),
                Value::U16(column.position),
                Value::Bool(column.nullable),
                Value::Bool(column.is_unique),
                Value::Bool(column.is_primary_key),
                column.default_val.as_ref().map(|v| crate::databox::Value::String(v.to_string())).unwrap_or(crate::databox::Value::Null),
            ];
            let col_bytes = sys_columns_layout.encode_tuple(&sys_columns_cols, &col_vals);
            heap::insert(&*self.bp, crate::common::constants::SYS_TABLE_COLUMNS_FID, &txn, &col_bytes).await?;
            self.bp.flush_all_dirty().await.map_err(Error::BufferError)?;
            Ok(())
        }
    }

    fn rename_column(
        &self,
        txn: Txn,
        table_oid: aliases::OId,
        column_oid: aliases::OId,
        new_name: String,
    ) -> impl Future<Output = Result<()>> + '_ + Send {
        async move {
            {
                let mut cache = self.catalog.write().expect("catalog lock poisoned");
                cache.rename_column(table_oid, column_oid, &new_name, &txn)?;
            }

            use futures::StreamExt;
            let sys_columns_fid = crate::common::constants::SYS_TABLE_COLUMNS_FID;
            let columns_layout = crate::databox::TupleLayout::from(catalog::schema::SYS_COLUMNS_COLUMNS_TABLE.to_vec());
            let mut stream = std::pin::pin!(heap::scan(&*self.bp, sys_columns_fid, txn.clone()).await?);

            while let Some(result) = stream.next().await {
                let (tuple_bytes, rid) = result?;
                if let Some(oid) = columns_layout.read_field("oid", 0, &tuple_bytes).and_then(|v| v.as_u32()) {
                    if oid == column_oid {
                        heap::delete(&*self.bp, sys_columns_fid, &txn, rid).await?;
                        let col = {
                            let cache = self.catalog.read().unwrap();
                            cache.get_table_columns(&txn, table_oid)?.into_iter().find(|c| c.oid == column_oid).unwrap()
                        };
                        let sys_columns_cols = ["oid", "table_oid", "name", "type_id", "position", "nullable", "is_unique", "is_primary_key", "default_val"];
                        let col_vals = [
                            Value::U32(col.oid),
                            Value::U32(col.table_oid),
                            Value::String(col.name.to_string()),
                            Value::U8(col.type_id as u8),
                            Value::U16(col.position),
                            Value::Bool(col.nullable),
                            Value::Bool(col.is_unique),
                            Value::Bool(col.is_primary_key),
                            col.default_val.as_ref().map(|v| crate::databox::Value::String(v.to_string())).unwrap_or(crate::databox::Value::Null),
                        ];
                        
                        let new_bytes = columns_layout.encode_tuple(&sys_columns_cols, &col_vals);
                        heap::insert(&*self.bp, sys_columns_fid, &txn, &new_bytes).await?;
                        break;
                    }
                }
            }
            self.bp.flush_all_dirty().await.map_err(Error::BufferError)?;
            Ok(())
        }
    }

    fn catalog_get_table_indexes(
        &self,
        txn: Txn,
        table_oid: aliases::OId,
    ) -> Result<Vec<catalog::Index>> {
        let cache = self.catalog.read().expect("catalog lock poisoned");
        cache.get_table_indexes(&txn, table_oid)
    }

    fn create_index(
        &self,
        txn: Txn,
        index: catalog::Index,
    ) -> impl Future<Output = Result<()>> + '_ + Send {
        async move {
            use crate::buffer::PageWriteGuard;
            use crate::common::constants::PAGE_BUF_SIZE;
            use crate::page::overlays::file_header::IndexFileHeaderPage;

            let file_id = index.file_id;
            let index_oid = index.oid;
            let header_loc = aliases::PPageId { file: file_id, offset: 0 };
            let header_lpage_id = self.bp.init_page_at_loc(header_loc).await.map_err(Error::BufferError)?;

            let mut guard = self.bp.fetch_page_at_loc_write(header_loc).await.map_err(Error::BufferError)?;
            let buffer = &mut *guard;
            buffer.fill(0);

            let temp_buffer = IndexFileHeaderPage::init([0u8; PAGE_BUF_SIZE], header_lpage_id, index_oid, index.table_oid);
            buffer.copy_from_slice(temp_buffer.as_buffer());
            guard.mark_dirty().map_err(Error::BufferError)?;
            drop(guard);

            let mut mem_index = index.clone();
            mem_index.xmin = txn.id;
            mem_index.xmax = 0;
            self.register_index(mem_index)?;

            let sys_indexes_layout = crate::databox::TupleLayout::from(catalog::schema::SYS_COLUMNS_INDEXES_TABLE.to_vec());
            let sys_indexes_cols = ["oid", "name", "table_oid", "column_oid", "file_id"];
            let sys_indexes_vals = [
                crate::databox::Value::U32(index.oid),
                crate::databox::Value::String(index.name.to_string()),
                crate::databox::Value::U32(index.table_oid),
                crate::databox::Value::U32(index.column_oid),
                crate::databox::Value::U32(index.file_id),
            ];
            let tuple_bytes = sys_indexes_layout.encode_tuple(&sys_indexes_cols, &sys_indexes_vals);
            heap::insert(&*self.bp, crate::common::constants::SYS_TABLE_INDEXES_FID, &txn, &tuple_bytes).await?;
            self.bp.flush_all_dirty().await.map_err(Error::BufferError)?;
            Ok(())
        }
    }

    fn drop_index(
        &self,
        txn: Txn,
        index_oid: aliases::OId,
        index_name: String,
    ) -> impl Future<Output = Result<()>> + '_ + Send {
        async move {
            {
                let mut cache = self.catalog.write().expect("catalog lock poisoned");
                cache.drop_index(&index_name, index_oid, &txn);
            }

            use futures::StreamExt;
            let sys_indexes_fid = crate::common::constants::SYS_TABLE_INDEXES_FID;
            let indexes_layout = crate::databox::TupleLayout::from(catalog::schema::SYS_COLUMNS_INDEXES_TABLE.to_vec());
            let mut stream = std::pin::pin!(heap::scan(&*self.bp, sys_indexes_fid, txn.clone()).await?);

            while let Some(result) = stream.next().await {
                let (tuple_bytes, rid) = result?;
                if let Some(oid) = indexes_layout.read_field("oid", 0, &tuple_bytes).and_then(|v| v.as_u32()) {
                    if oid == index_oid {
                        heap::delete(&*self.bp, sys_indexes_fid, &txn, rid).await?;
                        break;
                    }
                }
            }
            self.bp.flush_all_dirty().await.map_err(Error::BufferError)?;
            Ok(())
        }
    }

    fn register_fk(&self, child_table_oid: aliases::OId, fk: crate::accessor::FkInfo) {
        let mut cache = self.catalog.write().expect("catalog lock poisoned");
        cache.register_fk(child_table_oid, fk);
    }

    fn get_fks_for(&self, child_table_oid: aliases::OId) -> Vec<crate::accessor::FkInfo> {
        let cache = self.catalog.read().expect("catalog lock poisoned");
        cache.get_fks_for(child_table_oid)
    }

    fn get_fks_referencing(
        &self,
        parent_table_oid: aliases::OId,
    ) -> Vec<(aliases::OId, crate::accessor::FkInfo)> {
        let cache = self.catalog.read().expect("catalog lock poisoned");
        cache.get_fks_referencing(parent_table_oid)
    }
}