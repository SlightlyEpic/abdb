use std::borrow::Cow;

use crate::{buffer, common::aliases, databox};

pub enum Error {
    NotFound(String),
    BufferError(buffer::Error),
}

pub type Result<V> = std::result::Result<V, Error>;

#[derive(Clone, Debug)]
pub struct Table {
    pub oid: aliases::OId,
    pub name: Cow<'static, str>,
    pub file_id: aliases::FileId,
    pub xmin: aliases::TxnId,
    pub xmax: aliases::TxnId,
}

#[derive(Clone, Debug)]
pub struct Column {
    pub oid: aliases::OId,
    pub table_oid: aliases::OId,
    pub name: Cow<'static, str>,
    pub type_id: databox::DataType,
    pub position: u16,
    pub nullable: bool,
    pub is_unique: bool,
    pub is_primary_key: bool,
    pub default_val: Option<databox::Value>,
    pub xmin: aliases::TxnId,
    pub xmax: aliases::TxnId,
}

#[derive(Clone, Debug)]
pub struct Index {
    pub oid: aliases::OId,
    pub name: Cow<'static, str>,
    pub table_oid: aliases::OId,
    pub file_id: aliases::FileId,
    pub column_oid: aliases::OId,
    pub xmin: aliases::TxnId,
    pub xmax: aliases::TxnId,
}