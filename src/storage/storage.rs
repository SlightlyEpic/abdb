use std::{io, sync::Arc};

use crate::{
    common::aliases,
    storage::{allocator, directory},
};

#[derive(Clone, Debug)]
pub enum Error {
    IOError(Arc<io::Error>),
    DirectoryError(directory::Error),
    AllocatorError(allocator::Error),
}

pub type Result<V> = std::result::Result<V, Error>;

pub trait Storage: Send + Sync + 'static {
    type Error: std::fmt::Debug;

    fn read_page<'a>(
        &self,
        id: aliases::PageId,
        target: &'a mut aliases::PageBuffer,
    ) -> impl Future<Output = Result<()>> + '_ + Send;

    fn write_page<'a>(
        &self,
        id: aliases::PageId,
        target: &'a aliases::PageBuffer,
    ) -> impl Future<Output = Result<()>> + '_ + Send;

    fn read_page_at_loc<'a>(
        &self,
        loc: aliases::PhysicalId,
        target: &'a mut aliases::PageBuffer,
    ) -> impl Future<Output = Result<()>> + '_ + Send;

    fn write_page_at_loc<'a>(
        &self,
        loc: aliases::PhysicalId,
        target: &'a aliases::PageBuffer,
    ) -> impl Future<Output = Result<()>> + '_ + Send;

    
    fn new_page<'a>(&self) -> impl Future<Output = Result<aliases::PageId>> + '_ + Send;
}
