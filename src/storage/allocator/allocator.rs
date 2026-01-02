use crate::common::aliases;

#[derive(Clone, Copy, Debug)]
pub enum Error {
    // TODO: Page Allocator errors
}

pub type Result<V> = std::result::Result<V, Error>;

pub trait PageAllocator: Send + Sync + 'static {
    fn new_page(&self) -> impl Future<Output = Result<aliases::PhysicalId>> + '_ + Send;
    fn delete_page(
        &self,
        physical_id: aliases::PhysicalId,
    ) -> impl Future<Output = Result<()>> + '_ + Send;
}
