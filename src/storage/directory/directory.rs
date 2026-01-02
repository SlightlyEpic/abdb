use crate::common::aliases;

#[derive(Clone, Copy, Debug)]
pub enum Error {
    // TODO: Page Directory errors
}

pub type Result<V> = std::result::Result<V, Error>;

pub trait PageDirectory: Send + Sync + 'static {
    fn lookup(
        &self,
        page_id: aliases::PageId,
    ) -> impl Future<Output = Result<aliases::PhysicalId>> + '_ + Send;
    fn add_page(
        &self,
        page_id: aliases::PageId,
        physical_id: aliases::PhysicalId,
    ) -> impl Future<Output = Result<()>> + '_ + Send;
    fn update_page(
        &self,
        page_id: aliases::PageId,
        physical_id: aliases::PhysicalId,
    ) -> impl Future<Output = Result<()>> + '_ + Send;
    fn delete_page(&self, page_id: aliases::PageId)
    -> impl Future<Output = Result<()>> + '_ + Send;

    /// Force flush changes to disk
    fn flush_all_dirty(&self) -> impl Future<Output = Result<()>> + Send;
}
