//! Database lifecycle management: creation, opening, and bootstrapping.
//!
//! This module handles:
//! - Creating a new database (initializing system table files)
//! - Opening an existing database (loading catalog from system tables)
//! - DDL persistence (writing to system tables)

use std::borrow::Cow;
use std::path::Path;

use crate::{
    accessor::{AccessorImpl, heap},
    buffer::{BufferPool, PageWriteGuard},
    catalog::{self, schema, Column, Index, Table},
    common::{
        aliases::{FileId, OId, PPageId},
        constants,
        txn::Txn,
    },
    databox::{DataType, TupleLayout, Value},
    error::{DbError, Result},
    page::overlays::file_header::HeapFileHeaderPage,
    storage::{DiskManagerImpl, FileType, allocator::PageAllocator, directory::PageDirectory},
};

/// Marker file to indicate a database has been initialized.
const DB_MARKER_FILE: &str = ".abdb_init_marker";

/// Check if a database exists at the given path.
pub fn database_exists(path: &Path) -> bool {
    path.join(DB_MARKER_FILE).exists()
}

/// Initialize a new heap file by creating and writing the file header.
///
/// This must be called before any heap operations can be performed on the file.
pub async fn init_heap_file<B: BufferPool>(
    bp: &B,
    file_id: FileId,
    table_oid: OId,
) -> Result<()> {
    let header_loc = PPageId { file: file_id, offset: 0 };
    let header_lpage_id = bp.init_page_at_loc(header_loc).await.map_err(|e| DbError::Internal(format!("failed to init header loc: {:?}", e)))?;
    let mut guard = bp.fetch_page_at_loc_write(header_loc).await.map_err(|e| DbError::Internal(format!("failed to fetch header page: {:?}", e)))?;
    let buffer = &mut *guard;
    buffer.fill(0);
    let temp_buffer = HeapFileHeaderPage::init([0u8; constants::PAGE_BUF_SIZE], header_lpage_id, table_oid);
    buffer.copy_from_slice(temp_buffer.as_buffer());
    guard.mark_dirty().map_err(|e| DbError::Internal(format!("failed to mark header dirty: {:?}", e)))?;
    Ok(())
}

/// Bootstrap a new database by creating system table files.
///
/// This initializes:
/// - sys_tables heap file
/// - sys_columns heap file
/// - sys_indexes heap file
///
/// And inserts bootstrap records for the system tables themselves.
pub async fn bootstrap_database<B, D, A>(
    bp: &B,
    disk_manager: &DiskManagerImpl<D, A>,
    _accessor: &AccessorImpl<B>,
) -> Result<()>
where
    B: BufferPool,
    D: PageDirectory,
    A: PageAllocator,
{
    // Register system table files with the disk manager
    disk_manager.register_file(constants::SYS_TABLE_TABLES_FID, FileType::Heap);
    disk_manager.register_file(constants::SYS_TABLE_COLUMNS_FID, FileType::Heap);
    disk_manager.register_file(constants::SYS_TABLE_INDEXES_FID, FileType::Heap);

    // Initialize heap file headers
    init_heap_file(bp, constants::SYS_TABLE_TABLES_FID, constants::SYS_TABLE_TABLES_OID).await?;
    init_heap_file(bp, constants::SYS_TABLE_COLUMNS_FID, constants::SYS_TABLE_COLUMNS_OID).await?;
    init_heap_file(bp, constants::SYS_TABLE_INDEXES_FID, constants::SYS_TABLE_INDEXES_OID).await?;

    let temp_tm = std::sync::Arc::new(crate::transaction::TransactionManager::new());
    let txn = temp_tm.begin(crate::common::txn::IsolationLevel::Snapshot);

    // Insert bootstrap records for system tables
    insert_sys_table_record(bp, &txn, &schema::SYS_TABLE_TABLES).await?;
    insert_sys_table_record(bp, &txn, &schema::SYS_TABLE_COLUMNS).await?;
    insert_sys_table_record(bp, &txn, &schema::SYS_TABLE_INDEXES).await?;

    for col in schema::SYS_COLUMNS_TABLES_TABLE { insert_sys_column_record(bp, &txn, col).await?; }
    for col in schema::SYS_COLUMNS_COLUMNS_TABLE { insert_sys_column_record(bp, &txn, col).await?; }
    for col in schema::SYS_COLUMNS_INDEXES_TABLE { insert_sys_column_record(bp, &txn, col).await?; }

    bp.flush_all_dirty().await.map_err(|e| DbError::Internal(format!("failed to flush pages: {:?}", e)))?;
    Ok(())
}

async fn insert_sys_table_record<B: BufferPool>(bp: &B, txn: &Txn, table: &Table) -> Result<()> {
    let layout = TupleLayout::from(schema::SYS_COLUMNS_TABLES_TABLE.to_vec());
    let cols = ["oid", "name", "file_id"];
    let vals = [
        Value::U32(table.oid),
        Value::String(table.name.to_string()),
        Value::U32(table.file_id),
    ];
    let tuple_bytes = layout.encode_tuple(&cols, &vals);
    heap::insert(bp, constants::SYS_TABLE_TABLES_FID, txn, &tuple_bytes)
        .await
        .map_err(|e| DbError::Internal(format!("failed to insert sys_tables record: {:?}", e)))?;
    Ok(())
}

async fn insert_sys_column_record<B: BufferPool>(bp: &B, txn: &Txn, column: &Column) -> Result<()> {
    let layout = TupleLayout::from(schema::SYS_COLUMNS_COLUMNS_TABLE.to_vec());
    let cols = ["oid", "table_oid", "name", "type_id", "position", "nullable", "is_unique", "is_primary_key", "default_val"];
    let vals = [
        Value::U32(column.oid),
        Value::U32(column.table_oid),
        Value::String(column.name.to_string()),
        Value::U8(column.type_id as u8),
        Value::U16(column.position),
        Value::Bool(column.nullable),
        Value::Bool(column.is_unique),
        Value::Bool(column.is_primary_key),
        column.default_val.as_ref().map(|v| Value::String(v.to_string())).unwrap_or(Value::Null),
    ];
    let tuple_bytes = layout.encode_tuple(&cols, &vals);
    heap::insert(bp, constants::SYS_TABLE_COLUMNS_FID, txn, &tuple_bytes)
        .await
        .map_err(|e| DbError::Internal(format!("failed to insert sys_columns record: {:?}", e)))?;
    Ok(())
}

pub async fn load_catalog<B, D, A>(
    bp: &B,
    disk_manager: &DiskManagerImpl<D, A>,
    accessor: &AccessorImpl<B>,
) -> Result<crate::common::aliases::TxnId>
where
    B: BufferPool,
    D: PageDirectory,
    A: PageAllocator,
{
    use futures::StreamExt;
    use std::pin::pin;

    let mut max_xmin: crate::common::aliases::TxnId = 0;
    for sys_fid in [constants::SYS_TABLE_TABLES_FID, constants::SYS_TABLE_COLUMNS_FID, constants::SYS_TABLE_INDEXES_FID] {
        let x = heap::max_xmin(bp, sys_fid).await.map_err(|e| DbError::Internal(format!("max_xmin sys fid {}: {:?}", sys_fid, e)))?;
        if x > max_xmin { max_xmin = x; }
    }

    let temp_tm = std::sync::Arc::new(crate::transaction::TransactionManager::with_next_txn_id(u64::MAX));
    let txn = temp_tm.begin(crate::common::txn::IsolationLevel::Snapshot);

    let tables_layout = TupleLayout::from(schema::SYS_COLUMNS_TABLES_TABLE.to_vec());
    let stream = heap::scan(bp, constants::SYS_TABLE_TABLES_FID, txn.clone())
        .await
        .map_err(|e| DbError::Internal(format!("failed to scan sys_tables: {:?}", e)))?;

    let mut stream = pin!(stream);
    let mut user_tables: Vec<Table> = Vec::new();

    while let Some(result) = stream.next().await {
        let (tuple_bytes, _rid) = result.map_err(|e| DbError::Internal(format!("error reading sys_tables: {:?}", e)))?;
        let oid = tables_layout.read_field("oid", 0, &tuple_bytes).and_then(|v| v.as_u32()).unwrap_or(0);
        let name = tables_layout.read_field("name", 1, &tuple_bytes).and_then(|v| v.as_string()).unwrap_or_default();
        let file_id = tables_layout.read_field("file_id", 2, &tuple_bytes).and_then(|v| v.as_u32()).unwrap_or(0);
        if oid > constants::SYS_TABLE_INDEXES_OID {
            user_tables.push(Table { oid, name: Cow::Owned(name), file_id, xmin: 0, xmax: 0 });
        }
    }

    let columns_layout = TupleLayout::from(schema::SYS_COLUMNS_COLUMNS_TABLE.to_vec());
    let stream = heap::scan(bp, constants::SYS_TABLE_COLUMNS_FID, txn.clone())
        .await
        .map_err(|e| DbError::Internal(format!("failed to scan sys_columns: {:?}", e)))?;

    let mut stream = pin!(stream);
    let mut user_columns: Vec<Column> = Vec::new();

    while let Some(result) = stream.next().await {
        let (tuple_bytes, _rid) = result.map_err(|e| DbError::Internal(format!("error reading sys_columns: {:?}", e)))?;
        let type_id_raw = columns_layout.read_field("type_id", 3, &tuple_bytes).and_then(|v| v.as_u8()).unwrap_or(0);
        let type_id = DataType::from_u8(type_id_raw);
        let default_val_str = columns_layout.read_field("default_val", 8, &tuple_bytes).and_then(|v| v.as_string());
        let default_val = default_val_str.and_then(|s| type_id.parse_string(&s));
        let oid = columns_layout.read_field("oid", 0, &tuple_bytes).and_then(|v| v.as_u32()).unwrap_or(0);
        let table_oid = columns_layout.read_field("table_oid", 1, &tuple_bytes).and_then(|v| v.as_u32()).unwrap_or(0);
        let name = columns_layout.read_field("name", 2, &tuple_bytes).and_then(|v| v.as_string()).unwrap_or_default();
        let position = columns_layout.read_field("position", 4, &tuple_bytes).and_then(|v| v.as_u16()).unwrap_or(0);
        let nullable = columns_layout.read_field("nullable", 5, &tuple_bytes).and_then(|v| v.as_bool()).unwrap_or(false);
        let is_unique = columns_layout.read_field("is_unique", 6, &tuple_bytes).and_then(|v| v.as_bool()).unwrap_or(false);
        let is_primary_key = columns_layout.read_field("is_primary_key", 7, &tuple_bytes).and_then(|v| v.as_bool()).unwrap_or(false);

        if table_oid > constants::SYS_TABLE_INDEXES_OID {
            user_columns.push(Column {
                oid, table_oid, name: Cow::Owned(name), type_id: DataType::from_u8(type_id_raw),
                position, nullable, is_unique, is_primary_key, default_val,
                xmin: 0, xmax: 0,
            });
        }
    }

    let indexes_layout = TupleLayout::from(schema::SYS_COLUMNS_INDEXES_TABLE.to_vec());
    let stream = heap::scan(bp, constants::SYS_TABLE_INDEXES_FID, txn.clone())
        .await
        .map_err(|e| DbError::Internal(format!("failed to scan sys_indexes: {:?}", e)))?;

    let mut stream = pin!(stream);
    let mut user_indexes: Vec<Index> = Vec::new();

    while let Some(result) = stream.next().await {
        let (tuple_bytes, _rid) = result.map_err(|e| DbError::Internal(format!("error reading sys_indexes: {:?}", e)))?;
        let oid = indexes_layout.read_field("oid", 0, &tuple_bytes).and_then(|v| v.as_u32()).unwrap_or(0);
        let name = indexes_layout.read_field("name", 1, &tuple_bytes).and_then(|v| v.as_string()).unwrap_or_default();
        let table_oid = indexes_layout.read_field("table_oid", 2, &tuple_bytes).and_then(|v| v.as_u32()).unwrap_or(0);
        let column_oid = indexes_layout.read_field("column_oid", 3, &tuple_bytes).and_then(|v| v.as_u32()).unwrap_or(0);
        let file_id = indexes_layout.read_field("file_id", 4, &tuple_bytes).and_then(|v| v.as_u32()).unwrap_or(0);

        user_indexes.push(Index { oid, name: Cow::Owned(name), table_oid, column_oid, file_id, xmin: 0, xmax: 0 });
    }

    for table in user_tables {
        let table_oid = table.oid;
        let file_id = table.file_id;
        disk_manager.register_file(file_id, FileType::Heap);
        let cols: Vec<Column> = user_columns.iter().filter(|c| c.table_oid == table_oid).cloned().collect();
        accessor.register_table(table, cols).map_err(|e| DbError::Internal(format!("failed to register table: {:?}", e)))?;
        let x = heap::max_xmin(bp, file_id).await.map_err(|e| DbError::Internal(format!("max_xmin fid {}: {:?}", file_id, e)))?;
        if x > max_xmin { max_xmin = x; }
    }

    for index in user_indexes {
        disk_manager.register_file(index.file_id, FileType::Index);
        accessor.register_index(index).map_err(|e| DbError::Internal(format!("failed to register index: {:?}", e)))?;
    }

    Ok(max_xmin)
}

/// Write the database marker file to indicate initialization is complete.
pub fn write_marker_file(path: &Path) -> Result<()> {
    std::fs::write(path.join(DB_MARKER_FILE), "initialized")
        .map_err(|e| DbError::Internal(format!("failed to write marker file: {}", e)))?;
    Ok(())
}
