use crate::{
    buffer::{PageReadGuard, PageWriteGuard},
    common::aliases,
    storage, wal,
};

#[derive(Clone, Debug)]
pub enum Error {
    OOM,
    WALError(wal::Error),
    StorageError(storage::DiskError),
}

pub type Result<V> = std::result::Result<V, Error>;

pub trait BufferPool: Send + Sync + 'static {
    // The RAII guard for reading (Shared Latch)
    type ReadGuard<'a>: PageReadGuard
    where
        Self: 'a;

    // The RAII guard for writing (Exclusive Latch)
    type WriteGuard<'a>: PageWriteGuard
    where
        Self: 'a;

    /// Fetches a page for WRITING.
    /// 1. Checks if page is in memory.
    /// 2. If not, reads from Storage (Async I/O).
    /// 3. Pins the frame.
    /// 4. Acquires an Exclusive Latch (lock) on the page.
    fn fetch_page_write(
        &self,
        page_id: aliases::LPageId,
    ) -> impl Future<Output = Result<Self::WriteGuard<'_>>> + Send;

    /// Fetches a page for READING.
    /// (Similar to above, but acquires a Shared Latch)
    fn fetch_page_read(
        &self,
        page_id: aliases::LPageId,
    ) -> impl Future<Output = Result<Self::ReadGuard<'_>>> + Send;

    fn fetch_page_at_loc_write(
        &self,
        loc: aliases::PPageId,
    ) -> impl Future<Output = Result<Self::WriteGuard<'_>>> + Send;

    fn fetch_page_at_loc_read(
        &self,
        loc: aliases::PPageId,
    ) -> impl Future<Output = Result<Self::ReadGuard<'_>>> + Send;

    fn new_page(&self) -> impl Future<Output = Result<Self::WriteGuard<'_>>> + Send;

    fn flush_all_dirty(&self) -> impl Future<Output = Result<()>> + Send;
}
