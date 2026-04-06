use std::ops::{Deref, DerefMut};

use crate::common::aliases;

pub trait PageReadGuard: Deref<Target = aliases::PageBuffer> + Send {
    /// Returns the logical page ID for this page.
    fn lpage_id(&self) -> aliases::LPageId;
}

pub trait PageWriteGuard: DerefMut<Target = aliases::PageBuffer> + Send {
    type PageReadGuard: PageReadGuard;

    /// Returns the logical page ID for this page.
    fn lpage_id(&self) -> aliases::LPageId;

    /// Updates the page's PageLSN.
    /// This must be called before the guard is dropped if modifications were made.
    /// It ensures the WAL invariant: DataLSN <= LogLSN.
    fn commit_wal(&mut self, lsn: aliases::Lsn) -> impl Future<Output = super::Result<()>>;

    fn mark_dirty(&mut self) -> super::Result<()>;

    fn downgrade(self) -> Self::PageReadGuard;
}
