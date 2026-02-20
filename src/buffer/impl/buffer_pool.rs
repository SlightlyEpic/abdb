// TODO: Verify safety, readers and writers are not distinguishable

use std::cell::UnsafeCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU8, Ordering};

use crate::{buffer, storage};
use crate::buffer::evictor::EvictionPolicy;
use crate::buffer::r#impl::PageReadGuard;
use crate::common::{aliases, constants};
use crate::storage::DiskManager;

#[derive(Clone, Copy)]
enum FrameMeta {
    Vacant,
    Loaded {
        lpage_id: aliases::LPageId,
        ppage_id: aliases::PPageId,
        dirty: bool,
    }
}

pub struct BufferPool<D: DiskManager> {
    disk_manager: D,
    pub eviction_policy: Box<dyn EvictionPolicy>,
    num_frames: usize,
    buf: UnsafeCell<storage::AlignedBuffer>,

    pin_counts: Vec<AtomicU8>,
    frame_meta: std::sync::RwLock<Vec<FrameMeta>>,
    
    vacant_frames: std::sync::RwLock<HashSet<usize>>,
    frame_lpage_id_map: std::sync::RwLock<HashMap<aliases::LPageId, usize>>,
    frame_ppage_id_map: std::sync::RwLock<HashMap<aliases::PPageId, usize>>,

    frame_latches: Vec<tokio::sync::RwLock<()>>,
}

/// Safety: UnsafeCell is !Sync by default
unsafe impl<D: DiskManager + Sync> Sync for BufferPool<D> {}

impl<D: DiskManager> BufferPool<D> {
    pub fn new(num_frames: usize, disk_manager: D, eviction_policy: Box<dyn EvictionPolicy>) -> Self {
        Self {
            disk_manager,
            eviction_policy,
            num_frames,
            buf: UnsafeCell::new(storage::AlignedBuffer::new(num_frames * constants::PAGE_BUF_SIZE)),

            pin_counts: (0..num_frames).map(|_| AtomicU8::new(0)).collect(),
            frame_meta: std::sync::RwLock::new((0..num_frames).map(|_| FrameMeta::Vacant).collect()),

            vacant_frames: std::sync::RwLock::new((0..num_frames).collect()),
            frame_lpage_id_map: std::sync::RwLock::new(HashMap::new()),
            frame_ppage_id_map: std::sync::RwLock::new(HashMap::new()),
            
            frame_latches: (0..num_frames).map(|_| tokio::sync::RwLock::new(())).collect(),
        }
    }

    pub fn flush_wal_upto(&self, lsn: aliases::Lsn) -> impl Future<Output = buffer::Result<()>> {
        async { todo!() }
    }

    /* #region rwlock unwrapped reader and writers */

    pub fn frame_meta_read(&self) -> std::sync::RwLockReadGuard<'_, Vec<FrameMeta>> {
        self.frame_meta.read().expect("frame_meta lock is poisoned")
    }

    pub fn frame_meta_write(&self) -> std::sync::RwLockWriteGuard<'_, Vec<FrameMeta>> {
        self.frame_meta.write().expect("frame_meta lock is poisoned")
    }

    pub fn vacant_frames_read(&self) -> std::sync::RwLockReadGuard<'_, HashSet<usize>> {
        self.vacant_frames.read().expect("vacant_frames lock is poisoned")
    }

    pub fn vacant_frames_write(&self) -> std::sync::RwLockWriteGuard<'_, HashSet<usize>> {
        self.vacant_frames.write().expect("vacant_frames lock is poisoned")
    }

    pub fn frame_lpage_id_map_read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<aliases::LPageId, usize>> {
        self.frame_lpage_id_map.read().expect("frame_lpage_id_map lock is poisoned")
    }

    pub fn frame_lpage_id_map_write(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<aliases::LPageId, usize>> {
        self.frame_lpage_id_map.write().expect("frame_lpage_id_map lock is poisoned")
    }

    pub fn frame_ppage_id_map_read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<aliases::PPageId, usize>> {
        self.frame_ppage_id_map.read().expect("frame_ppage_id_map lock is poisoned")
    }

    pub fn frame_ppage_id_map_write(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<aliases::PPageId, usize>> {
        self.frame_ppage_id_map.write().expect("frame_ppage_id_map lock is poisoned")
    }

    /* #endregion */

    fn incr_pin(&self, frame_idx: usize) {
        self.pin_counts[frame_idx].fetch_add(1, Ordering::SeqCst);
    }

    fn decr_pin(&self, frame_idx: usize) {
        self.pin_counts[frame_idx].fetch_sub(1, Ordering::SeqCst);
    }
    
    fn evict(&self) -> buffer::Result<()> {
    let victim = self.eviction_policy
            .find_victim()
            .map_err(|e| buffer::Error::EvictorError(e))?;

        let (victim_lpage_id, victim_ppage_id) = {
            let frame_meta_read = &self.frame_meta_read();
            match frame_meta_read[victim] {
                FrameMeta::Vacant => panic!("victim frame was vacant"),
                FrameMeta::Loaded { lpage_id, ppage_id, dirty } => (lpage_id, ppage_id),
            }
        };

        self.vacant_frames_write().insert(victim);
        self.frame_lpage_id_map_write().remove(&victim_lpage_id);
        self.frame_ppage_id_map_write().remove(&victim_ppage_id);
        self.frame_meta_write()[victim] = FrameMeta::Vacant;

        Ok(())
    }

    /// SAFETY: returns a mut slice - it is the responsibility of the caller to ensure that aliasing rules are followed
    unsafe fn frame_buf_mut(&self, frame_idx: usize) -> &mut aliases::PageBuffer {
        unsafe {
            let aligned_buf = &*self.buf.get();
            let base_ptr = aligned_buf.as_ptr();

            let start_offset = frame_idx * constants::PAGE_BUF_SIZE;
            let offset_ptr = base_ptr.add(start_offset);
            
            std::slice::from_raw_parts_mut(offset_ptr, constants::PAGE_BUF_SIZE)
        }
    }
}

impl<D: DiskManager> buffer::BufferPool for BufferPool<D> {
    type ReadGuard<'a>
        = super::PageReadGuard<'a, D>
    where
        Self: 'a;

    type WriteGuard<'a>
        = super::PageWriteGuard<'a, D>
    where
        Self: 'a;

    fn load_page_as_unevictable(
        &self,
        page_id: aliases::LPageId,
    ) -> impl Future<Output = ()> + Send {
        async { todo!() }
    }

    fn load_page_loc_as_unevictable(
        &self,
        loc: aliases::PPageId,
    ) -> impl Future<Output = ()> + Send {
        async { todo!() }
    }

    fn fetch_page_write(
        &self,
        page_id: aliases::LPageId,
    ) -> impl Future<Output = buffer::Result<Self::WriteGuard<'_>>> + Send {
        async { todo!() }
    }

    fn fetch_page_read(
        &'static self,
        page_id: aliases::LPageId,
    ) -> impl Future<Output = buffer::Result<Self::ReadGuard<'static>>> + Send {
        async move {
            let (frame_idx, is_loaded) = {
                let frame_lpage_id_map = self.frame_lpage_id_map_read();
                match frame_lpage_id_map.get(&page_id) {
                    Some(&frame_idx) => (frame_idx, true),
                    None => {
                        let num_vacant_frames = {
                            let lock = self.vacant_frames_read();
                            lock.capacity()
                        };
                        if num_vacant_frames == 0 {
                            self.evict()?
                        }

                        let vacant_idx = {
                            let mut lock = self.vacant_frames_write();
                            let idx = lock.iter().next().cloned().unwrap();
                            lock.remove(&idx);
                            idx
                        };

                        (vacant_idx, false)
                    }
                }
            };

            self.incr_pin(frame_idx);

            let frame_slice = unsafe { self.frame_buf_mut(frame_idx) };
            let latch = self.frame_latches[frame_idx].blocking_read();

            let ppage_id = self.disk_manager
                .read_page(page_id, frame_slice).await
                .map_err(|e| buffer::Error::StorageError(e))?;

            if !is_loaded {
                self.frame_meta_write()[frame_idx] = FrameMeta::Loaded {
                    lpage_id: page_id,
                    ppage_id: ppage_id,
                    dirty: false,
                };
                self.frame_lpage_id_map_write().insert(page_id, frame_idx);
                self.frame_ppage_id_map_write().insert(ppage_id, frame_idx);
            }

            Ok(PageReadGuard::new(
                frame_idx, 
                frame_slice, 
                self,
                latch,
            ))
        }
    }

    fn fetch_page_at_loc_write(
        &self,
        loc: aliases::PPageId,
    ) -> impl Future<Output = buffer::Result<Self::WriteGuard<'_>>> + Send {
        async { todo!() }
    }

    fn fetch_page_at_loc_read(
        &self,
        loc: aliases::PPageId,
    ) -> impl Future<Output = buffer::Result<Self::ReadGuard<'_>>> + Send {
        async { todo!() }
    }

    fn new_page(&self) -> impl Future<Output = buffer::Result<Self::WriteGuard<'_>>> + Send {
        async { todo!() }
    }

    fn flush_all_dirty(&self) -> impl Future<Output = buffer::Result<()>> + Send {
        async { todo!() }
    }
}
