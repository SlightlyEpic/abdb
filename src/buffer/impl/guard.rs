use std::ops::{Deref, DerefMut};

use crate::{
    buffer::{self, r#impl::BufferPool},
    common::aliases,
    storage::DiskManager,
};

// * Pin Guard

pub struct PinGuard<'a, D: DiskManager> {
    pub(crate) frame_idx: usize,
    buffer_pool: &'a BufferPool<D>,
}

impl<'a, D: DiskManager> PinGuard<'a, D> {
    /// Get the logical page ID for this frame.
    pub fn lpage_id(&self) -> aliases::LPageId {
        self.buffer_pool.frame_lpage_id(self.frame_idx)
    }
}

impl<'a, D: DiskManager> Drop for PinGuard<'a, D> {
    fn drop(&mut self) {
        self.buffer_pool.decr_pin(self.frame_idx);
    }
}

// * Read Guard

pub struct PageReadGuard<'a, D: DiskManager> {
    pub(crate) _pin_guard: PinGuard<'a, D>,
    page: &'a aliases::PageBuffer,
    _latch_guard: tokio::sync::RwLockReadGuard<'a, ()>,
}

impl<'a, D: DiskManager> PageReadGuard<'a, D> {
    pub fn new(
        frame_idx: usize,
        page: &'a aliases::PageBuffer,
        buffer_pool: &'a BufferPool<D>,
        latch_guard: tokio::sync::RwLockReadGuard<'a, ()>,
    ) -> Self {
        buffer_pool.eviction_policy.record_access(frame_idx);
        Self {
            _pin_guard: PinGuard {
                frame_idx,
                buffer_pool,
            },
            page,
            _latch_guard: latch_guard,
        }
    }

    /// Get the logical page ID for this page.
    pub fn lpage_id(&self) -> aliases::LPageId {
        self._pin_guard.lpage_id()
    }
}

impl<'a, D: DiskManager> Deref for PageReadGuard<'a, D> {
    type Target = aliases::PageBuffer;

    fn deref(&self) -> &Self::Target {
        self.page
    }
}

// * Write Guard

pub struct PageWriteGuard<'a, D: DiskManager> {
    pub(crate) _pin_guard: PinGuard<'a, D>,
    page: &'a mut aliases::PageBuffer,
    _latch_guard: tokio::sync::RwLockWriteGuard<'a, ()>,
}

impl<'a, D: DiskManager> PageWriteGuard<'a, D> {
    pub fn new(
        frame_idx: usize,
        page: &'a mut aliases::PageBuffer,
        buffer_pool: &'a BufferPool<D>,
        latch_guard: tokio::sync::RwLockWriteGuard<'a, ()>,
    ) -> Self {
        Self {
            _pin_guard: PinGuard {
                frame_idx,
                buffer_pool,
            },
            page,
            _latch_guard: latch_guard,
        }
    }

    /// Get the logical page ID for this page.
    pub fn lpage_id(&self) -> aliases::LPageId {
        self._pin_guard.lpage_id()
    }
}

impl<'a, D: DiskManager> Deref for PageWriteGuard<'a, D> {
    type Target = aliases::PageBuffer;

    fn deref(&self) -> &Self::Target {
        self.page
    }
}

impl<'a, D: DiskManager> DerefMut for PageWriteGuard<'a, D> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.page
    }
}

impl<'a, D: DiskManager> buffer::PageReadGuard for PageReadGuard<'a, D> {
    fn lpage_id(&self) -> aliases::LPageId {
        self._pin_guard.lpage_id()
    }
}

impl<'a, D: DiskManager> buffer::PageWriteGuard for PageWriteGuard<'a, D> {
    type PageReadGuard = PageReadGuard<'a, D>;

    fn lpage_id(&self) -> aliases::LPageId {
        self._pin_guard.lpage_id()
    }

    fn commit_wal(
        &mut self,
        lsn: crate::common::aliases::Lsn,
    ) -> impl Future<Output = Result<(), buffer::Error>> {
        self._pin_guard.buffer_pool.flush_wal_upto(lsn)
    }

    fn mark_dirty(&mut self) -> buffer::Result<()> {
        self._pin_guard
            .buffer_pool
            .mark_frame_dirty(self._pin_guard.frame_idx)
    }

    fn downgrade(self) -> Self::PageReadGuard {
        Self::PageReadGuard {
            _pin_guard: self._pin_guard,
            page: self.page,
            _latch_guard: self._latch_guard.downgrade(),
        }
    }
}
