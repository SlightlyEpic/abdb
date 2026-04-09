use std::collections::HashMap;

use crate::{
    accessor::visibility::is_visible_mvcc, catalog::{self, Column, Index, Table, schema}, common::{aliases::OId, constants, txn::Txn}
};

use super::accessor::{Error, FkInfo, Result};

pub(super) struct CatalogCache {
    tables: HashMap<OId, Table>,
    // Map a name to historical versions of OIDs
    tables_by_name: HashMap<String, Vec<OId>>,
    // Map table_oid to all historical versions of its columns
    columns: HashMap<OId, Vec<Column>>,
    indexes: HashMap<OId, Index>,
    indexes_by_name: HashMap<String, Vec<OId>>,
    // In-memory only (not persisted): FKs declared on a child table_oid.
    foreign_keys: HashMap<OId, Vec<FkInfo>>,
}

impl CatalogCache {
    pub fn new() -> Self {
        let mut cache = Self {
            tables: HashMap::new(),
            tables_by_name: HashMap::new(),
            columns: HashMap::new(),
            indexes: HashMap::new(),
            indexes_by_name: HashMap::new(),
            foreign_keys: HashMap::new(),
        };

        cache.register_table(schema::SYS_TABLE_TABLES.clone()).unwrap();
        cache.register_columns(constants::SYS_TABLE_TABLES_OID, schema::SYS_COLUMNS_TABLES_TABLE.to_vec());

        cache.register_table(schema::SYS_TABLE_COLUMNS.clone()).unwrap();
        cache.register_columns(constants::SYS_TABLE_COLUMNS_OID, schema::SYS_COLUMNS_COLUMNS_TABLE.to_vec());

        cache.register_table(schema::SYS_TABLE_INDEXES.clone()).unwrap();
        cache.register_columns(constants::SYS_TABLE_INDEXES_OID, schema::SYS_COLUMNS_INDEXES_TABLE.to_vec());

        cache
    }

    pub fn register_table(&mut self, table: Table) -> Result<()> {
        let name = table.name.to_string();
        self.tables_by_name.entry(name).or_default().push(table.oid);
        self.tables.insert(table.oid, table);
        Ok(())
    }

    pub fn register_columns(&mut self, table_oid: OId, columns: Vec<Column>) {
        self.columns.entry(table_oid).or_default().extend(columns);
    }

    pub fn register_index(&mut self, index: Index) -> Result<()> {
        let name = index.name.to_string();
        self.indexes_by_name.entry(name).or_default().push(index.oid);
        self.indexes.insert(index.oid, index);
        Ok(())
    }

    pub fn get_table_by_name(&self, txn: &Txn, name: &str) -> Result<catalog::Table> {
        let oids = self.tables_by_name.get(name).ok_or_else(|| Error::NotFound(format!("table '{}'", name)))?;
        for &oid in oids {
            if let Some(table) = self.tables.get(&oid) {
                if is_visible_mvcc(table.xmin, table.xmax, txn) {
                    return Ok(table.clone());
                }
            }
        }
        Err(Error::NotFound(format!("table '{}'", name)))
    }

    pub fn get_table_by_oid(&self, txn: &Txn, oid: OId) -> Result<catalog::Table> {
        let table = self.tables.get(&oid).ok_or_else(|| Error::NotFound(format!("table oid {}", oid)))?;
        if is_visible_mvcc(table.xmin, table.xmax, txn) {
            Ok(table.clone())
        } else {
            Err(Error::NotFound(format!("table oid {}", oid)))
        }
    }

    pub fn get_index_by_name(&self, txn: &Txn, name: &str) -> Result<catalog::Index> {
        let oids = self.indexes_by_name.get(name).ok_or_else(|| Error::NotFound(format!("index '{}'", name)))?;
        for &oid in oids {
            if let Some(idx) = self.indexes.get(&oid) {
                if is_visible_mvcc(idx.xmin, idx.xmax, txn) {
                    return Ok(idx.clone());
                }
            }
        }
        Err(Error::NotFound(format!("index '{}'", name)))
    }

    pub fn get_index_by_oid(&self, txn: &Txn, oid: OId) -> Result<catalog::Index> {
        let idx = self.indexes.get(&oid).ok_or_else(|| Error::NotFound(format!("index oid {}", oid)))?;
        if is_visible_mvcc(idx.xmin, idx.xmax, txn) {
            Ok(idx.clone())
        } else {
            Err(Error::NotFound(format!("index oid {}", oid)))
        }
    }

    pub fn get_table_columns(&self, txn: &Txn, table_oid: OId) -> Result<Vec<catalog::Column>> {
        let cols = self.columns.get(&table_oid).ok_or_else(|| Error::NotFound(format!("columns for table oid {}", table_oid)))?;
        let visible_cols: Vec<_> = cols.iter()
            .filter(|c| is_visible_mvcc(c.xmin, c.xmax, txn))
            .cloned()
            .collect();
        Ok(visible_cols)
    }

    pub fn drop_table(&mut self, _name: &str, oid: OId, txn: &Txn) {
        if let Some(table) = self.tables.get_mut(&oid) {
            table.xmax = txn.id;
        }
        if let Some(cols) = self.columns.get_mut(&oid) {
            for col in cols.iter_mut() {
                col.xmax = txn.id;
            }
        }
        for index in self.indexes.values_mut() {
            if index.table_oid == oid {
                index.xmax = txn.id;
            }
        }
    }

    pub fn add_column(&mut self, table_oid: OId, mut column: Column, txn: &Txn) -> Result<()> {
        column.xmin = txn.id;
        column.xmax = 0;
        if let Some(cols) = self.columns.get_mut(&table_oid) {
            cols.push(column);
            Ok(())
        } else {
            Err(Error::NotFound(format!("table oid {}", table_oid)))
        }
    }

    pub fn rename_column(&mut self, table_oid: OId, column_oid: OId, new_name: &str, txn: &Txn) -> Result<()> {
        if let Some(cols) = self.columns.get_mut(&table_oid) {
            let mut old_col_clone = None;
            for col in cols.iter_mut() {
                if col.oid == column_oid && is_visible_mvcc(col.xmin, col.xmax, txn) {
                    col.xmax = txn.id;
                    old_col_clone = Some(col.clone());
                    break;
                }
            }
            if let Some(mut new_col) = old_col_clone {
                new_col.name = std::borrow::Cow::Owned(new_name.to_string());
                new_col.xmin = txn.id;
                new_col.xmax = 0;
                cols.push(new_col);
                return Ok(());
            }
        }
        Err(Error::NotFound(format!("column oid {}", column_oid)))
    }

    pub fn drop_index(&mut self, _name: &str, oid: OId, txn: &Txn) {
        if let Some(idx) = self.indexes.get_mut(&oid) {
            idx.xmax = txn.id;
        }
    }

    pub fn get_table_indexes(&self, txn: &Txn, table_oid: OId) -> Result<Vec<catalog::Index>> {
        Ok(self.indexes.values()
            .filter(|i| i.table_oid == table_oid && is_visible_mvcc(i.xmin, i.xmax, txn))
            .cloned()
            .collect())
    }

    pub fn register_fk(&mut self, child_table_oid: OId, fk: FkInfo) {
        self.foreign_keys.entry(child_table_oid).or_default().push(fk);
    }

    pub fn get_fks_for(&self, child_table_oid: OId) -> Vec<FkInfo> {
        self.foreign_keys
            .get(&child_table_oid)
            .cloned()
            .unwrap_or_default()
    }

    pub fn get_fks_referencing(&self, parent_table_oid: OId) -> Vec<(OId, FkInfo)> {
        let mut out = Vec::new();
        for (child_oid, fks) in self.foreign_keys.iter() {
            for fk in fks {
                if fk.parent_table_oid == parent_table_oid {
                    out.push((*child_oid, fk.clone()));
                }
            }
        }
        out
    }

    pub fn get_all_tables(&self, txn: &Txn) -> Vec<catalog::Table> {
        self.tables.values()
            .filter(|t| is_visible_mvcc(t.xmin, t.xmax, txn))
            .cloned()
            .collect()
    }
}