use std::borrow::Cow;

use crate::{buffer, common::aliases, databox};

pub enum Error {
    NotFound(String),
    BufferError(buffer::Error),
}

pub type Result<V> = std::result::Result<V, Error>;

#[derive(Clone)]
pub struct Table {
    pub oid: aliases::OId,
    pub name: Cow<'static, str>,
    pub file_id: aliases::FileId,
}

#[derive(Clone)]
pub struct Column {
    pub oid: aliases::OId,
    pub table_oid: aliases::OId,
    pub name: Cow<'static, str>,
    pub type_id: databox::DataType,
    pub position: u16,
    pub nullable: bool,
}

#[derive(Clone)]
pub struct Index {
    pub oid: aliases::OId,
    pub name: Cow<'static, str>,
    pub table_oid: aliases::OId,
    pub file_id: aliases::FileId,
    // TODO: Support multi-column indexes later
    // Should be simple, just needs another index columns table with dynamic layout calculation
    pub column_oid: aliases::OId,
}
