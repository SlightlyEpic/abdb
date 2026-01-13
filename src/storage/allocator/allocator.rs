use std::sync::Arc;

use futures::io;

use crate::common::aliases::{FileId, PPageId};

#[derive(Clone, Debug)]
pub enum Error {
    IOError(Arc<io::Error>),
}

pub type Result<V> = std::result::Result<V, Error>;

/// This component handles tracking allocation and deallocation of pages in files
/// It may also expand/truncate files when appropriate
pub trait PageAllocator: Send + Sync + 'static {
    fn allocate(&self, file_id: FileId) -> impl Future<Output = Result<PPageId>> + Send;
    fn deallocate(&self, physical_id: PPageId) -> impl Future<Output = Result<()>> + Send;
}
