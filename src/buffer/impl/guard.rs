use std::ops::{Deref, DerefMut};

use crate::{
    buffer::{self, r#impl::BufferPool},
    common::aliases, storage::DiskManager,
};

// * Read Guard

pub struct PageReadGuard<'a, D: DiskManager> {
    frame_idx: usize,
    page: &'a aliases::PageBuffer,
    buffer_pool: &'static BufferPool<D>,
    _latch_guard: tokio::sync::RwLockReadGuard<'a, ()>,
}

impl<'a, D: DiskManager> PageReadGuard<'a, D> {
    pub fn new(
        frame_idx: usize,
        page: &'a aliases::PageBuffer,
        buffer_pool: &'static BufferPool<D>,
        latch_guard: tokio::sync::RwLockReadGuard<'a, ()>,
    ) -> Self {
        buffer_pool.incr_pin(frame_idx);
        buffer_pool.eviction_policy.record_access(frame_idx);
        Self {
            frame_idx,
            page,
            buffer_pool,
            _latch_guard: latch_guard,
        }
    }
}

impl<'a, D: DiskManager> Drop for PageReadGuard<'a, D> {
    fn drop(&mut self) {
        self.buffer_pool.decr_pin(self.frame_idx);
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
    frame_idx: usize,
    page: &'a mut aliases::PageBuffer,
    buffer_pool: &'static BufferPool<D>,
    _latch_guard: tokio::sync::RwLockWriteGuard<'a, ()>,
}

impl<'a, D: DiskManager> PageWriteGuard<'a, D> {
    pub fn new(
        frame_idx: usize,
        page: &'a mut aliases::PageBuffer,
        buffer_pool: &'static BufferPool<D>,
        latch_guard: tokio::sync::RwLockWriteGuard<'a, ()>,
    ) -> Self {
        buffer_pool.incr_pin(frame_idx);
        Self {
            frame_idx,
            page,
            buffer_pool,
            _latch_guard: latch_guard,
        }
    }
}

impl<'a, D: DiskManager> Drop for PageWriteGuard<'a, D> {
    fn drop(&mut self) {
        self.buffer_pool.decr_pin(self.frame_idx);
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

impl<'a, D: DiskManager> buffer::PageReadGuard for PageReadGuard<'a, D> {}
impl<'a, D: DiskManager> buffer::PageWriteGuard for PageWriteGuard<'a, D> {
    fn commit_wal(
        &mut self,
        lsn: crate::common::aliases::Lsn,
    ) -> impl Future<Output = Result<(), buffer::Error>> {
        self.buffer_pool.flush_wal_upto(lsn)
    }

    fn mark_dirty(&mut self) -> buffer::Result<()> {
        todo!()
    }
}
