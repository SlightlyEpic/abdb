use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, RwLock};

use futures::{Stream, StreamExt};

use crate::{
    buffer::BufferPool,
    catalog,
    common::{aliases, constants, txn::Txn},
    databox::{DataType, TupleLayout, Value},
    page::overlays::file_header::HeapFileHeaderPage,
    page::overlays::file_header::IndexFileHeaderPage,
};

use super::{
    accessor::{Accessor, Error, NewColumn, Result},
    btree,
    catalog_cache::CatalogCache,
    heap,
    visibility,
};

/// First file id and oid the accessor will hand out for user objects.
/// Kept well above all reserved sys-table file ids / oids and the page
/// directory file id sentinel to avoid collisions.
const FIRST_USER_FID: aliases::FileId = 100;
const FIRST_USER_OID: aliases::OId = 100;

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
    /// Base directory for on-disk files. Mirrors the path the
    /// `DiskManagerImpl` was constructed with so DDL can create/remove
    /// data files via the filesystem directly.
    base_path: PathBuf,
    next_file_id: AtomicU32,
    next_oid: AtomicU32,
}

impl<B: BufferPool> AccessorImpl<B> {
    /// Create a new AccessorImpl with the given buffer pool.
    ///
    /// `base_path` must be the same directory the underlying `DiskManagerImpl`
    /// is reading and writing under, since DDL creates and removes data
    /// files in that directory directly.
    ///
    /// Initializes the catalog cache with system table definitions.
    /// In a production system, this would also scan the system tables
    /// to load user-created tables and indexes.
    pub fn new(bp: Arc<B>, base_path: PathBuf) -> Self {
        Self {
            bp,
            catalog: RwLock::new(CatalogCache::new()),
            base_path,
            next_file_id: AtomicU32::new(FIRST_USER_FID),
            next_oid: AtomicU32::new(FIRST_USER_OID),
        }
    }

    fn alloc_file_id(&self) -> aliases::FileId {
        self.next_file_id.fetch_add(1, Ordering::SeqCst)
    }

    fn alloc_oid(&self) -> aliases::OId {
        self.next_oid.fetch_add(1, Ordering::SeqCst)
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

    /// Best-effort compensating cleanup for a partially-completed
    /// `create_table`. Used because we don't yet have a real per-txn undo
    /// log; on any failure mid-way through DDL we manually undo every step
    /// that already succeeded. All errors here are swallowed — the original
    /// failure is what gets returned to the caller.
    async fn cleanup_failed_create_table(
        &self,
        table_oid: aliases::OId,
        file_id: aliases::FileId,
        txn: &Txn,
        col_rids: &[aliases::RecordId],
        sys_tables_rid: Option<aliases::RecordId>,
    ) {
        for rid in col_rids {
            let _ = heap::delete(&*self.bp, constants::SYS_TABLE_COLUMNS_FID, txn, *rid).await;
        }
        if let Some(rid) = sys_tables_rid {
            let _ = heap::delete(&*self.bp, constants::SYS_TABLE_TABLES_FID, txn, rid).await;
        }
        let _ = delete_heap_file(&self.base_path, file_id);
        let mut cache = self.catalog.write().expect("catalog lock poisoned");
        cache.deregister_table(table_oid);
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

    fn catalog_get_table_by_name(&self, _txn: Txn, table_name: String) -> Result<catalog::Table> {
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

    fn catalog_get_index_by_name(&self, _txn: Txn, index_name: String) -> Result<catalog::Index> {
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

    // -- DDL ----------------------------------------------------------------

    fn create_table(
        &self,
        txn: Txn,
        name: String,
        columns: Vec<NewColumn>,
    ) -> impl Future<Output = Result<catalog::Table>> + '_ + Send {
        async move {
            // ── Phase 1: dup-check + alloc + cache register, all under one
            //    write lock so two concurrent create_table("foo") can't race.
            //    No awaits here ∴ holding the std::sync::RwLock is sound.
            let (table, cat_columns) = {
                let mut cache = self.catalog.write().expect("catalog lock poisoned");
                if let Ok(existing) = cache.get_table_by_name(&name) {
                    return Err(Error::DuplicateOId(existing.oid));
                }

                let table_oid = self.alloc_oid();
                let file_id = self.alloc_file_id();

                let cat_columns: Vec<catalog::Column> = columns
                    .iter()
                    .enumerate()
                    .map(|(i, c)| catalog::Column {
                        oid: self.alloc_oid(),
                        table_oid,
                        name: Cow::Owned(c.name.clone()),
                        type_id: c.type_id,
                        position: i as u16,
                        nullable: c.nullable,
                    })
                    .collect();

                let table = catalog::Table {
                    oid: table_oid,
                    name: Cow::Owned(name),
                    file_id,
                };
                cache.register_table(table.clone())?;
                cache.register_columns(table_oid, cat_columns.clone());
                (table, cat_columns)
            };

            // From here on, any failure must compensate by undoing prior steps:
            // best-effort since we have no real txn undo log yet.
            let table_oid = table.oid;
            let file_id = table.file_id;
            let table_name = table.name.to_string();

            // ── Phase 2: create heap file on disk.
            if let Err(e) = create_heap_file(&self.base_path, file_id, table_oid) {
                self.cleanup_failed_create_table(table_oid, file_id, &txn, &[], None)
                    .await;
                return Err(e);
            }

            // ── Phase 3: persist sys_tables row.
            let sys_tables_layout = sys_tables_layout();
            let row = serialize_row(
                &sys_tables_layout,
                &sys_table_columns(),
                &[
                    Value::U32(table_oid),
                    Value::String(table_name.clone()),
                    Value::U32(file_id),
                ],
            );
            let sys_tables_rid = match heap::insert(
                &*self.bp,
                constants::SYS_TABLE_TABLES_FID,
                &txn,
                &row,
            )
            .await
            {
                Ok(rid) => rid,
                Err(e) => {
                    self.cleanup_failed_create_table(table_oid, file_id, &txn, &[], None)
                        .await;
                    return Err(e);
                }
            };

            // ── Phase 4: persist one sys_columns row per column. Track RIDs
            //    so we can soft-delete them if a later insert fails.
            let sys_cols_layout = sys_columns_layout();
            let sys_cols_schema = sys_columns_columns();
            let mut col_rids: Vec<aliases::RecordId> = Vec::with_capacity(cat_columns.len());
            for col in &cat_columns {
                let row = serialize_row(
                    &sys_cols_layout,
                    &sys_cols_schema,
                    &[
                        Value::U32(col.oid),
                        Value::U32(col.table_oid),
                        Value::String(col.name.to_string()),
                        Value::U8(col.type_id.into()),
                        Value::U16(col.position),
                        Value::Bool(col.nullable),
                    ],
                );
                match heap::insert(&*self.bp, constants::SYS_TABLE_COLUMNS_FID, &txn, &row).await {
                    Ok(rid) => col_rids.push(rid),
                    Err(e) => {
                        self.cleanup_failed_create_table(
                            table_oid,
                            file_id,
                            &txn,
                            &col_rids,
                            Some(sys_tables_rid),
                        )
                        .await;
                        return Err(e);
                    }
                }
            }

            Ok(table)
        }
    }

    fn drop_table(
        &self,
        txn: Txn,
        table_oid: aliases::OId,
    ) -> impl Future<Output = Result<()>> + '_ + Send {
        async move {
            // Snapshot info we need before mutating the cache.
            let (file_id, table_name) = {
                let cache = self.catalog.read().expect("catalog lock poisoned");
                if cache.has_index_for_table(table_oid) {
                    return Err(Error::CapacityExceeded(format!(
                        "table oid {} still has indexes — drop them first",
                        table_oid
                    )));
                }
                let t = cache.get_table_by_oid(table_oid)?;
                (t.file_id, t.name.to_string())
            };

            // 1. Delete the row in sys_tables that matches our table_oid.
            delete_sys_row_by_u32(
                &*self.bp,
                &txn,
                constants::SYS_TABLE_TABLES_FID,
                &sys_table_columns(),
                "oid",
                table_oid,
            )
            .await?;

            // 2. Delete every sys_columns row whose table_oid matches.
            delete_sys_row_by_u32(
                &*self.bp,
                &txn,
                constants::SYS_TABLE_COLUMNS_FID,
                &sys_columns_columns(),
                "table_oid",
                table_oid,
            )
            .await?;

            // 3. Drop from cache.
            {
                let mut cache = self.catalog.write().expect("catalog lock poisoned");
                cache.deregister_table(table_oid);
            }

            // 4. Physically remove the heap file from disk. Any frames the
            //    buffer pool still caches for this file_id are now orphaned;
            //    they will never be re-read because the catalog entry is gone.
            //    Eviction would attempt to write back which is a known
            //    limitation flagged in docs/progress.md.
            let _ = table_name; // reserved for future logging
            delete_heap_file(&self.base_path, file_id)?;
            Ok(())
        }
    }

    fn create_index(
        &self,
        txn: Txn,
        name: String,
        table_oid: aliases::OId,
        column_oid: aliases::OId,
    ) -> impl Future<Output = Result<catalog::Index>> + '_ + Send {
        async move {
            // Verify the table exists.
            {
                let cache = self.catalog.read().expect("catalog lock poisoned");
                cache.get_table_by_oid(table_oid)?;
                if cache.get_index_by_name(&name).is_ok() {
                    return Err(Error::DuplicateOId(0));
                }
            }

            let index_oid = self.alloc_oid();
            let file_id = self.alloc_file_id();

            create_index_file(&self.base_path, file_id, index_oid, table_oid)?;

            let index = catalog::Index {
                oid: index_oid,
                name: Cow::Owned(name.clone()),
                table_oid,
                file_id,
                column_oid,
            };
            {
                let mut cache = self.catalog.write().expect("catalog lock poisoned");
                cache.register_index(index.clone())?;
            }

            // Persist into sys_indexes.
            let layout = sys_indexes_layout();
            let row = serialize_row(
                &layout,
                &sys_indexes_columns(),
                &[
                    Value::U32(index_oid),
                    Value::String(name.clone()),
                    Value::U32(table_oid),
                    Value::U32(column_oid),
                    Value::U32(file_id),
                ],
            );
            heap::insert(&*self.bp, constants::SYS_TABLE_INDEXES_FID, &txn, &row).await?;

            Ok(index)
        }
    }

    fn drop_index(
        &self,
        txn: Txn,
        index_oid: aliases::OId,
    ) -> impl Future<Output = Result<()>> + '_ + Send {
        async move {
            let file_id = {
                let cache = self.catalog.read().expect("catalog lock poisoned");
                cache.get_index_by_oid(index_oid)?.file_id
            };

            delete_sys_row_by_u32(
                &*self.bp,
                &txn,
                constants::SYS_TABLE_INDEXES_FID,
                &sys_indexes_columns(),
                "oid",
                index_oid,
            )
            .await?;

            {
                let mut cache = self.catalog.write().expect("catalog lock poisoned");
                cache.deregister_index(index_oid);
            }

            delete_index_file(&self.base_path, file_id)?;
            Ok(())
        }
    }
}

// ============================================================================
// FILE LIFECYCLE HELPERS (kept inside the accessor module to avoid touching
// the storage / buffer trait surfaces; the disk manager opens files lazily
// via OpenOptions::create(true) so creating a heap/index file is just a
// matter of writing an initialised header page at offset 0)
// ============================================================================

fn heap_file_path(base: &PathBuf, file_id: aliases::FileId) -> PathBuf {
    base.join(format!("{}.heap", file_id))
}

fn index_file_path(base: &PathBuf, file_id: aliases::FileId) -> PathBuf {
    base.join(format!("{}.idx", file_id))
}

fn create_heap_file(
    base: &PathBuf,
    file_id: aliases::FileId,
    table_oid: aliases::OId,
) -> Result<()> {
    use std::fs::OpenOptions;
    use std::os::unix::fs::FileExt;

    let path = heap_file_path(base, file_id);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .map_err(|e| Error::PageCorruption(format!("create heap file {:?}: {}", path, e)))?;

    let mut buf = [0u8; constants::PAGE_BUF_SIZE];
    HeapFileHeaderPage::init(&mut buf[..], 0, table_oid);
    file.write_all_at(&buf, 0)
        .map_err(|e| Error::PageCorruption(format!("write heap header {:?}: {}", path, e)))?;
    Ok(())
}

fn create_index_file(
    base: &PathBuf,
    file_id: aliases::FileId,
    index_oid: aliases::OId,
    table_oid: aliases::OId,
) -> Result<()> {
    use std::fs::OpenOptions;
    use std::os::unix::fs::FileExt;

    let path = index_file_path(base, file_id);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .map_err(|e| Error::PageCorruption(format!("create index file {:?}: {}", path, e)))?;

    let mut buf = [0u8; constants::PAGE_BUF_SIZE];
    IndexFileHeaderPage::init(&mut buf[..], 0, index_oid, table_oid);
    file.write_all_at(&buf, 0)
        .map_err(|e| Error::PageCorruption(format!("write index header {:?}: {}", path, e)))?;
    Ok(())
}

fn delete_heap_file(base: &PathBuf, file_id: aliases::FileId) -> Result<()> {
    let path = heap_file_path(base, file_id);
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| Error::PageCorruption(format!("remove heap file {:?}: {}", path, e)))?;
    }
    Ok(())
}

fn delete_index_file(base: &PathBuf, file_id: aliases::FileId) -> Result<()> {
    let path = index_file_path(base, file_id);
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| Error::PageCorruption(format!("remove index file {:?}: {}", path, e)))?;
    }
    Ok(())
}

// ============================================================================
// SYS-TABLE SCHEMA HELPERS
// ============================================================================

fn sys_table_columns() -> Vec<catalog::Column> {
    catalog::schema::SYS_COLUMNS_TABLES_TABLE.to_vec()
}
fn sys_columns_columns() -> Vec<catalog::Column> {
    catalog::schema::SYS_COLUMNS_COLUMNS_TABLE.to_vec()
}
fn sys_indexes_columns() -> Vec<catalog::Column> {
    catalog::schema::SYS_COLUMNS_INDEXES_TABLE.to_vec()
}

fn sys_tables_layout() -> TupleLayout {
    sys_table_columns().into()
}
fn sys_columns_layout() -> TupleLayout {
    sys_columns_columns().into()
}
fn sys_indexes_layout() -> TupleLayout {
    sys_indexes_columns().into()
}

// ============================================================================
// MINIMAL TUPLE SERIALIZER
// (TupleLayout::write_field handles fixed-width types but bails on String;
//  full serialization including the variable-length string heap is local to
//  the accessor module since no other caller currently needs it)
// ============================================================================

/// Serialize a row into the heap-tuple representation expected by `heap::insert`,
/// i.e. **without** the leading 16-byte MVCC header (heap::insert prepends its
/// own). Field offsets in the returned bytes are layout-relative starting at
/// position 16 (the layout treats the first 16 bytes as the MVCC header).
fn serialize_row(
    layout: &TupleLayout,
    columns: &[catalog::Column],
    values: &[Value],
) -> Vec<u8> {
    debug_assert_eq!(columns.len(), values.len());

    let mut sorted: Vec<&catalog::Column> = columns.iter().collect();
    sorted.sort_by_key(|c| c.position);

    let fixed_len = layout.fixed_len as usize;
    let mut buf = vec![0u8; fixed_len];
    let mut heap = Vec::<u8>::new();

    for col in &sorted {
        let i = col.position as usize;
        let val = &values[i];
        let off = *layout
            .offsets
            .get(&*col.name)
            .expect("column not in layout") as usize;

        match (&col.type_id, val) {
            (DataType::String, Value::String(s)) => {
                let len = s.len() as u16;
                let abs_off = (fixed_len + heap.len()) as u16;
                buf[off..off + 2].copy_from_slice(&len.to_le_bytes());
                buf[off + 2..off + 4].copy_from_slice(&abs_off.to_le_bytes());
                heap.extend_from_slice(s.as_bytes());
            }
            (_, Value::Null) => {
                // Set null bitmap bit; field bytes stay zero.
                let bitmap_start = layout.null_bitmap_offset as usize;
                let byte_idx = bitmap_start + (i / 8);
                if byte_idx < buf.len() {
                    buf[byte_idx] |= 1u8 << (i % 8);
                }
            }
            _ => {
                layout.write_field(&col.name, i, &mut buf, val);
            }
        }
    }

    buf.extend_from_slice(&heap);
    // Strip MVCC header bytes — heap::insert prepends its own.
    buf.split_off(visibility::TUPLE_HEADER_SIZE)
}

// ============================================================================
// DELETE-BY-FIELD HELPER (used by drop_table / drop_index)
// ============================================================================

/// Scan a sys table and soft-delete every visible tuple whose `field_name`
/// (which must be `DataType::U32`) equals `target`. Heap deletes leave the
/// tuple in place but stamp XMAX, which is the standard MVCC pattern.
async fn delete_sys_row_by_u32<B: BufferPool>(
    bp: &B,
    txn: &Txn,
    file_id: aliases::FileId,
    columns: &[catalog::Column],
    field_name: &str,
    target: u32,
) -> Result<()> {
    let layout: TupleLayout = columns.to_vec().into();
    let stream = heap::scan(bp, file_id, txn.clone()).await?;
    let mut s = Box::pin(stream);

    // Collect the rids that match first to avoid mutating the heap mid-scan.
    let mut victims = Vec::new();
    while let Some(item) = s.next().await {
        let (user_data, rid) = item?;
        // The layout reads at offsets that include the 16-byte MVCC header,
        // so prepend a dummy header before calling read_field.
        let mut full = Vec::with_capacity(visibility::TUPLE_HEADER_SIZE + user_data.len());
        full.extend_from_slice(&[0u8; visibility::TUPLE_HEADER_SIZE]);
        full.extend_from_slice(&user_data);

        let col_index = columns
            .iter()
            .find(|c| &*c.name == field_name)
            .map(|c| c.position as usize)
            .ok_or_else(|| Error::NotFound(format!("sys col {}", field_name)))?;

        if let Some(Value::U32(v)) = layout.read_field(field_name, col_index, &full)
            && v == target
        {
            victims.push(rid);
        }
    }
    drop(s);

    for rid in victims {
        heap::delete(bp, file_id, txn, rid).await?;
    }
    Ok(())
}
