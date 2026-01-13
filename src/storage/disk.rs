use std::{io, sync::Arc};

use crate::{
    common::aliases,
    storage::{allocator, directory},
};

#[derive(Clone, Debug)]
pub enum DiskError {
    IOError(Arc<io::Error>),
    DirectoryError(directory::Error),
    AllocatorError(allocator::Error),
}

pub type Result<V> = std::result::Result<V, DiskError>;

pub trait DiskManager: Send + Sync + 'static {
    fn read_page<'a>(
        &self,
        id: aliases::LPageId,
        target: &'a mut aliases::PageBuffer,
    ) -> impl Future<Output = Result<()>> + '_ + Send;

    fn write_page<'a>(
        &self,
        id: aliases::LPageId,
        target: &'a aliases::PageBuffer,
    ) -> impl Future<Output = Result<()>> + '_ + Send;

    fn read_page_at_loc<'a>(
        &self,
        loc: aliases::PPageId,
        target: &'a mut aliases::PageBuffer,
    ) -> impl Future<Output = Result<()>> + '_ + Send;

    fn write_page_at_loc<'a>(
        &self,
        loc: aliases::PPageId,
        target: &'a aliases::PageBuffer,
    ) -> impl Future<Output = Result<()>> + '_ + Send;

    fn new_page(&self) -> impl Future<Output = Result<aliases::LPageId>> + '_ + Send;
    fn num_pages(&self) -> impl Future<Output = Result<u32>> + '_ + Send;
}
