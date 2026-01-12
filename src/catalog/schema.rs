use std::borrow::Cow;

use crate::{common::constants, databox::DataType};
use super::{Table, Column};

pub const SYS_TABLE_TABLES: Table = Table {
    oid: constants::SYS_TABLE_TABLES_OID,
    name: Cow::Borrowed("sys_tables"),
    file_id: constants::SYS_TABLE_TABLES_FID,
};

pub const SYS_TABLE_COLUMNS: Table = Table {
    oid: constants::SYS_TABLE_COLUMNS_OID,
    name: Cow::Borrowed("sys_columns"),
    file_id: constants::SYS_TABLE_COLUMNS_FID,
};

pub const SYS_TABLE_INDEXES: Table = Table {
    oid: constants::SYS_TABLE_INDEXES_OID,
    name: Cow::Borrowed("sys_indexes"),
    file_id: constants::SYS_TABLE_INDEXES_FID,
};

pub const SYS_COLUMNS_TABLES_TABLE: &[Column] = &[
    Column {
        oid: 00,
        table_oid: constants::SYS_TABLE_TABLES_OID,
        name: Cow::Borrowed("oid"),
        position: 0,
        type_id: DataType::U32,
        nullable: false,
    },
    Column {
        oid: 01,
        table_oid: constants::SYS_TABLE_TABLES_OID,
        name: Cow::Borrowed("name"),
        position: 1,
        type_id: DataType::String,
        nullable: false,
    },
    Column {
        oid: 02,
        table_oid: constants::SYS_TABLE_TABLES_OID,
        name: Cow::Borrowed("file_id"),
        position: 2,
        type_id: DataType::U32,
        nullable: false,
    },
];

pub const SYS_COLUMNS_COLUMNS_TABLE: &[Column] = &[
    Column {
        oid: 10,
        table_oid: constants::SYS_TABLE_COLUMNS_OID,
        name: Cow::Borrowed("oid"),
        position: 0,
        type_id: DataType::U32,
        nullable: false,
    },
    Column {
        oid: 11,
        table_oid: constants::SYS_TABLE_COLUMNS_OID,
        name: Cow::Borrowed("table_oid"),
        position: 1,
        type_id: DataType::U32,
        nullable: false,
    },
    Column {
        oid: 12,
        table_oid: constants::SYS_TABLE_COLUMNS_OID,
        name: Cow::Borrowed("name"),
        position: 2,
        type_id: DataType::String,
        nullable: false,
    },
    Column {
        oid: 13,
        table_oid: constants::SYS_TABLE_COLUMNS_OID,
        name: Cow::Borrowed("type_id"),
        position: 3,
        type_id: DataType::U8,
        nullable: false,
    },
    Column {
        oid: 14,
        table_oid: constants::SYS_TABLE_COLUMNS_OID,
        name: Cow::Borrowed("position"),
        position: 4,
        type_id: DataType::U16,
        nullable: false,
    },
    Column {
        oid: 15,
        table_oid: constants::SYS_TABLE_COLUMNS_OID,
        name: Cow::Borrowed("nullable"),
        position: 5,
        type_id: DataType::Bool,
        nullable: false,
    },
];

pub const SYS_COLUMNS_INDEXES_TABLE: &[Column] = &[
    Column {
        oid: 20,
        table_oid: constants::SYS_TABLE_INDEXES_OID,
        name: Cow::Borrowed("oid"),
        position: 0,
        type_id: DataType::U32,
        nullable: false,
    },
    Column {
        oid: 21,
        table_oid: constants::SYS_TABLE_INDEXES_OID,
        name: Cow::Borrowed("name"),
        position: 1,
        type_id: DataType::String,
        nullable: false,
    },
    Column {
        oid: 22,
        table_oid: constants::SYS_TABLE_INDEXES_OID,
        name: Cow::Borrowed("table_oid"),
        position: 2,
        type_id: DataType::U32,
        nullable: false,
    },
    Column {
        oid: 23,
        table_oid: constants::SYS_TABLE_INDEXES_OID,
        name: Cow::Borrowed("column_oid"),
        position: 3,
        type_id: DataType::U32,
        nullable: false,
    },
];
