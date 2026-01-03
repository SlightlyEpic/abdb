use std::sync::Arc;

use futures::io;

use crate::common::aliases::{PhysicalId, FileId};

#[derive(Clone, Debug)]
pub enum Error {
    IOError(Arc<io::Error>)
}

pub type Result<V> = std::result::Result<V, Error>;

pub trait PageAllocator: Send + Sync + 'static {
    fn allocate(&self, file_id: FileId) -> impl Future<Output = Result<PhysicalId>> + Send;
    fn deallocate(&self, physical_id: PhysicalId) -> impl Future<Output = Result<()>> + Send;
}
