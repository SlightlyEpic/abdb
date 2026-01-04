use std::ops::{Deref, DerefMut};

use crate::common::aliases;

pub trait PageReadGuard: Deref<Target = aliases::PageBuffer> + Send {}

pub trait PageWriteGuard: DerefMut<Target = aliases::PageBuffer> + Send {
    /// Updates the page's PageLSN.
    /// This must be called before the guard is dropped if modifications were made.
    /// It ensures the WAL invariant: DataLSN <= LogLSN.
    fn commit_wal(&mut self, lsn: aliases::Lsn);
}

// impl<'a> Drop for WriteGuard<'a> {
//     fn drop(&mut self) {
//         // 1. Release the RWLock/Latch on the frame
//         // 2. Decrement the Pin Count in the Buffer Pool
//         self.buffer_pool.unpin(self.frame_id);
//     }
// }
