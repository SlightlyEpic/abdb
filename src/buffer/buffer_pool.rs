use std::ops::{Deref, DerefMut};

use crate::{common::aliases, storage, wal};

#[derive(Clone, Debug)]
pub enum Error {
    OOM,
    WALError(wal::Error),
    StorageError(storage::Error),
}

pub type Result<V> = std::result::Result<V, Error>;

pub trait BufferPool: Send + Sync + 'static {
    type Error: std::fmt::Debug;

    // The RAII guard for reading (Shared Latch)
    type ReadGuard<'a>: Deref<Target = aliases::PageBuffer> + Send
    where
        Self: 'a;

    // The RAII guard for writing (Exclusive Latch)
    type WriteGuard<'a>: PageWriteGuard + Send
    where
        Self: 'a;

    /// Fetches a page for WRITING.
    /// 1. Checks if page is in memory.
    /// 2. If not, reads from Storage (Async I/O).
    /// 3. Pins the frame.
    /// 4. Acquires an Exclusive Latch (lock) on the page.
    fn fetch_page_write(
        &self,
        page_id: aliases::PageId,
    ) -> impl Future<Output = Result<Self::WriteGuard<'_>>> + Send;

    /// Fetches a page for READING.
    /// (Similar to above, but acquires a Shared Latch)
    fn fetch_page_read(
        &self,
        page_id: aliases::PageId,
    ) -> impl Future<Output = Result<Self::ReadGuard<'_>>> + Send;

    fn new_page(&self) -> impl Future<Output = Result<Self::WriteGuard<'_>>> + Send;

    fn flush_all_dirty(&self) -> impl Future<Output = Result<()>> + Send;
}

pub trait PageWriteGuard: DerefMut<Target = aliases::PageBuffer> {
    fn page_id(&self) -> aliases::PageId;
    fn is_dirty(&self);

    /// Updates the page's PageLSN.
    /// This must be called before the guard is dropped if modifications were made.
    /// It ensures the WAL invariant: DataLSN <= LogLSN.
    fn mark_dirty(&mut self, lsn: aliases::Lsn);
}

// impl<'a> Drop for WriteGuard<'a> {
//     fn drop(&mut self) {
//         // 1. Release the RWLock/Latch on the frame
//         // 2. Decrement the Pin Count in the Buffer Pool
//         self.buffer_pool.unpin(self.frame_id);
//     }
// }
