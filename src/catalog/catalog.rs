use std::borrow::Cow;

use crate::{buffer, common::aliases, databox};

pub enum Error {
    NotFound(String),
    BufferError(buffer::Error),
}

pub type Result<V> = std::result::Result<V, Error>;

pub struct Table {
    pub oid: aliases::OId,
    pub name: Cow<'static, str>,
    pub file_id: aliases::FileId,
}

pub struct Column {
    pub oid: aliases::OId,
    pub table_oid: aliases::OId,
    pub name: Cow<'static, str>,
    pub type_id: databox::DataType,
    pub position: u16,
    pub nullable: bool,
}

pub struct Index {
    pub oid: aliases::OId,
    pub name: Cow<'static, str>,
    pub table_oid: aliases::OId,
    // TODO: Support multi-column indexes later
    // Should be simple, just needs another index columns table with dynamic layout calculation
    pub column_oid: aliases::OId,
}
