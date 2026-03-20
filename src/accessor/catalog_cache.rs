use std::collections::HashMap;

use crate::{
    catalog::{self, schema, Column, Index, Table},
    common::{aliases::OId, constants},
};

use super::accessor::{Error, Result};

/// In-memory catalog cache.
///
/// Holds all table, column, and index metadata in HashMaps for O(1) lookup.
/// Populated at startup with system table definitions, then extended as
/// DDL operations create user tables and indexes.
///
/// The Accessor trait requires catalog operations to be synchronous (no Future),
/// which is why everything must be pre-loaded into memory.
pub(super) struct CatalogCache {
    tables: HashMap<OId, Table>,
    tables_by_name: HashMap<String, OId>,
    columns: HashMap<OId, Vec<Column>>,
    indexes: HashMap<OId, Index>,
    indexes_by_name: HashMap<String, OId>,
}

impl CatalogCache {
    /// Create a new catalog cache pre-populated with system tables.
    pub fn new() -> Self {
        let mut cache = Self {
            tables: HashMap::new(),
            tables_by_name: HashMap::new(),
            columns: HashMap::new(),
            indexes: HashMap::new(),
            indexes_by_name: HashMap::new(),
        };

        // Register system tables
        cache.register_table(schema::SYS_TABLE_TABLES.clone());
        cache.register_columns(
            constants::SYS_TABLE_TABLES_OID,
            schema::SYS_COLUMNS_TABLES_TABLE.to_vec(),
        );

        cache.register_table(schema::SYS_TABLE_COLUMNS.clone());
        cache.register_columns(
            constants::SYS_TABLE_COLUMNS_OID,
            schema::SYS_COLUMNS_COLUMNS_TABLE.to_vec(),
        );

        cache.register_table(schema::SYS_TABLE_INDEXES.clone());
        cache.register_columns(
            constants::SYS_TABLE_INDEXES_OID,
            schema::SYS_COLUMNS_INDEXES_TABLE.to_vec(),
        );

        cache
    }

    // ========================================================================
    // REGISTRATION (used by DDL and init)
    // ========================================================================

    pub fn register_table(&mut self, table: Table) {
        self.tables_by_name
            .insert(table.name.to_string(), table.oid);
        self.tables.insert(table.oid, table);
    }

    pub fn register_columns(&mut self, table_oid: OId, columns: Vec<Column>) {
        self.columns.insert(table_oid, columns);
    }

    pub fn register_index(&mut self, index: Index) {
        self.indexes_by_name
            .insert(index.name.to_string(), index.oid);
        self.indexes.insert(index.oid, index);
    }

    // ========================================================================
    // LOOKUP
    // ========================================================================

    pub fn get_table_by_name(&self, name: &str) -> Result<catalog::Table> {
        let oid = self
            .tables_by_name
            .get(name)
            .ok_or_else(|| Error::NotFound(format!("table '{}'", name)))?;
        self.tables
            .get(oid)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("table oid {}", oid)))
    }

    pub fn get_table_by_oid(&self, oid: OId) -> Result<catalog::Table> {
        self.tables
            .get(&oid)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("table oid {}", oid)))
    }

    pub fn get_index_by_name(&self, name: &str) -> Result<catalog::Index> {
        let oid = self
            .indexes_by_name
            .get(name)
            .ok_or_else(|| Error::NotFound(format!("index '{}'", name)))?;
        self.indexes
            .get(oid)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("index oid {}", oid)))
    }

    pub fn get_index_by_oid(&self, oid: OId) -> Result<catalog::Index> {
        self.indexes
            .get(&oid)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("index oid {}", oid)))
    }

    pub fn get_table_columns(&self, table_oid: OId) -> Result<Vec<catalog::Column>> {
        self.columns
            .get(&table_oid)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("columns for table oid {}", table_oid)))
    }
}
