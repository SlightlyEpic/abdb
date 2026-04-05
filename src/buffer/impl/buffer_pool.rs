use std::cell::UnsafeCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::buffer::PageWriteGuard as PageWriteGuardTrait;
use crate::buffer::evictor::EvictionPolicy;
use crate::buffer::r#impl::{PageReadGuard, PageWriteGuard};
use crate::common::{aliases, constants};
use crate::page::overlays;
use crate::storage::DiskManager;
use crate::{buffer, storage};

#[derive(Clone, Copy, Debug)]
enum FrameMeta {
    Vacant,
    Loaded {
        lpage_id: aliases::LPageId,
        ppage_id: aliases::PPageId,
        dirty: bool,
    },
}

pub struct BufferPool<D: DiskManager> {
    disk_manager: Arc<D>,
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
    pub fn new(
        num_frames: usize,
        disk_manager: Arc<D>,
        eviction_policy: Box<dyn EvictionPolicy>,
    ) -> Self {
        Self {
            disk_manager,
            eviction_policy,
            num_frames,
            buf: UnsafeCell::new(storage::AlignedBuffer::new(
                num_frames * constants::PAGE_BUF_SIZE,
            )),

            pin_counts: (0..num_frames).map(|_| AtomicU8::new(0)).collect(),
            frame_meta: std::sync::RwLock::new(
                (0..num_frames).map(|_| FrameMeta::Vacant).collect(),
            ),

            vacant_frames: std::sync::RwLock::new((0..num_frames).collect()),
            frame_lpage_id_map: std::sync::RwLock::new(HashMap::new()),
            frame_ppage_id_map: std::sync::RwLock::new(HashMap::new()),

            frame_latches: (0..num_frames)
                .map(|_| tokio::sync::RwLock::new(()))
                .collect(),
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
        self.frame_meta
            .write()
            .expect("frame_meta lock is poisoned")
    }

    pub fn vacant_frames_read(&self) -> std::sync::RwLockReadGuard<'_, HashSet<usize>> {
        self.vacant_frames
            .read()
            .expect("vacant_frames lock is poisoned")
    }

    pub fn vacant_frames_write(&self) -> std::sync::RwLockWriteGuard<'_, HashSet<usize>> {
        self.vacant_frames
            .write()
            .expect("vacant_frames lock is poisoned")
    }

    pub fn frame_lpage_id_map_read(
        &self,
    ) -> std::sync::RwLockReadGuard<'_, HashMap<aliases::LPageId, usize>> {
        self.frame_lpage_id_map
            .read()
            .expect("frame_lpage_id_map lock is poisoned")
    }

    pub fn frame_lpage_id_map_write(
        &self,
    ) -> std::sync::RwLockWriteGuard<'_, HashMap<aliases::LPageId, usize>> {
        self.frame_lpage_id_map
            .write()
            .expect("frame_lpage_id_map lock is poisoned")
    }

    pub fn frame_ppage_id_map_read(
        &self,
    ) -> std::sync::RwLockReadGuard<'_, HashMap<aliases::PPageId, usize>> {
        self.frame_ppage_id_map
            .read()
            .expect("frame_ppage_id_map lock is poisoned")
    }

    pub fn frame_ppage_id_map_write(
        &self,
    ) -> std::sync::RwLockWriteGuard<'_, HashMap<aliases::PPageId, usize>> {
        self.frame_ppage_id_map
            .write()
            .expect("frame_ppage_id_map lock is poisoned")
    }

    /* #endregion */

    pub fn incr_pin(&self, frame_idx: usize) {
        let old_pin = self.pin_counts[frame_idx].fetch_add(1, Ordering::SeqCst);
        if old_pin == 0 {
            self.eviction_policy.set_evictable(frame_idx, false);
        }
    }

    pub fn decr_pin(&self, frame_idx: usize) {
        let old_pin = self.pin_counts[frame_idx].fetch_sub(1, Ordering::SeqCst);
        if old_pin == 1 {
            self.eviction_policy.set_evictable(frame_idx, true);
        }
    }

    /// Mark a frame as dirty. Called by PageWriteGuard::mark_dirty.
    ///
    /// # Safety
    /// Caller must hold the write latch for this frame.
    pub fn mark_frame_dirty(&self, frame_idx: usize) -> buffer::Result<()> {
        let mut meta = self.frame_meta_write();
        match &mut meta[frame_idx] {
            FrameMeta::Loaded { dirty, .. } => {
                *dirty = true;
                Ok(())
            }
            FrameMeta::Vacant => Err(buffer::Error::SomeoneFuckedUp(
                "attempted to mark vacant frame as dirty".into(),
            )),
        }
    }

    async fn evict(&self) -> buffer::Result<()> {
        loop {
            // 1. Ask the policy for a victim
            let victim_idx = self
                .eviction_policy
                .find_victim()
                .map_err(|e| buffer::Error::EvictorError(e))?;

            // 2. Lock the victim frame exclusively.
            // This ensures no ongoing reads/writes are happening.
            let _write_latch = self.frame_latches[victim_idx].write().await;

            // 3. Double-check the pin count.
            // Another thread might have pinned this frame while we were waiting
            // for the write_latch. If so, we cannot evict it.
            if self.pin_counts[victim_idx].load(Ordering::SeqCst) != 0 {
                // The frame is in use. Tell the eviction policy it made a bad
                // choice (if your policy supports this) or just loop and try again.
                continue;
            }

            // 4. Read the metadata to see what we are evicting
            let (lpage_id, ppage_id, is_dirty) = {
                let meta_read = self.frame_meta_read();
                match meta_read[victim_idx] {
                    FrameMeta::Vacant => {
                        // Edge case: Policy returned an already vacant frame.
                        // Add it to the vacant list and return success.
                        self.vacant_frames_write().insert(victim_idx);
                        return Ok(());
                    }
                    FrameMeta::Loaded {
                        lpage_id,
                        ppage_id,
                        dirty,
                    } => (lpage_id, ppage_id, dirty),
                }
            };

            // 5. If dirty, write back to disk!
            if is_dirty {
                // Safe because we hold the exclusive write_latch for this frame
                let frame_slice = unsafe { &*self.frame_buf(victim_idx) };

                self.disk_manager
                    .write_page_at_loc(ppage_id, frame_slice) // Assuming your disk manager has this
                    .await
                    .map_err(|e| buffer::Error::StorageError(e))?;
            }

            // 6. Safely remove from all global maps and metadata
            {
                self.frame_lpage_id_map_write().remove(&lpage_id);
                self.frame_ppage_id_map_write().remove(&ppage_id);

                let mut meta_write = self.frame_meta_write();
                meta_write[victim_idx] = FrameMeta::Vacant;

                self.vacant_frames_write().insert(victim_idx);

                self.eviction_policy.remove_frame(victim_idx);
            }

            // Successfully evicted and cleaned up!
            return Ok(());
        }
    }

    /// SAFETY: Caller must ensure exclusive access (Write Latch) to the specific frame.
    unsafe fn frame_buf_mut(&self, frame_idx: usize) -> *mut aliases::PageBuffer {
        let base_ptr = self.buf.get() as *mut u8;
        let start_offset = frame_idx * constants::PAGE_BUF_SIZE;
        let offset_ptr = unsafe { base_ptr.add(start_offset) };

        std::ptr::slice_from_raw_parts_mut(offset_ptr, constants::PAGE_BUF_SIZE)
    }

    /// SAFETY: Caller must ensure at least shared access (Read Latch) to the specific frame.
    unsafe fn frame_buf(&self, frame_idx: usize) -> *const aliases::PageBuffer {
        let base_ptr = self.buf.get() as *const u8;
        let start_offset = frame_idx * constants::PAGE_BUF_SIZE;
        let offset_ptr = unsafe { base_ptr.add(start_offset) };

        std::ptr::slice_from_raw_parts(offset_ptr, constants::PAGE_BUF_SIZE)
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
    ) -> impl Future<Output = ()> + Send + '_ {
        async move {
            let read_guard = self.fetch_page_read(page_id).await.unwrap();
            // pin count will never go below 1, frame will never be evicted
            self.incr_pin(read_guard._pin_guard.frame_idx);
        }
    }

    fn load_page_loc_as_unevictable(
        &self,
        loc: aliases::PPageId,
    ) -> impl Future<Output = ()> + Send + '_ {
        async move {
            let read_guard = self.fetch_page_at_loc_read(loc).await.unwrap();
            // pin count will never go below 1, frame will never be evicted
            self.incr_pin(read_guard._pin_guard.frame_idx);
        }
    }

    fn fetch_page_write(
        &self,
        page_id: aliases::LPageId,
    ) -> impl Future<Output = buffer::Result<Self::WriteGuard<'_>>> + Send + '_ {
        async move {
            let (frame_idx, is_loaded, _opt_write_latch) = loop {
                {
                    let map = self.frame_lpage_id_map.read().unwrap();
                    if let Some(&idx) = map.get(&page_id) {
                        self.incr_pin(idx);
                        break (idx, true, None);
                    }
                }

                let vacant_idx_opt = {
                    let mut vacant = self.vacant_frames.write().unwrap();
                    if let Some(&idx) = vacant.iter().next() {
                        vacant.remove(&idx);
                        Some(idx)
                    } else {
                        None
                    }
                };

                let vacant_idx = match vacant_idx_opt {
                    Some(idx) => idx,
                    None => {
                        self.evict().await?;
                        continue;
                    }
                };

                self.incr_pin(vacant_idx);

                let write_latch = self.frame_latches[vacant_idx].write().await;

                {
                    let mut map = self.frame_lpage_id_map_write();

                    if let Some(&idx) = map.get(&page_id) {
                        // Someone else loaded it while we were evicting/yielding.
                        self.decr_pin(vacant_idx);
                        self.vacant_frames_write().insert(vacant_idx);
                        // write_latch is automatically dropped here when it goes out of scope

                        self.incr_pin(idx);
                        break (idx, true, None);
                    }

                    // Safe to publish. We already hold the write_latch!
                    map.insert(page_id, vacant_idx);
                }

                // Pass the acquired write_latch out of the loop
                break (vacant_idx, false, Some(write_latch));
            };

            if is_loaded {
                let write_latch = self.frame_latches[frame_idx].write().await;
                let frame_slice = unsafe { &mut *self.frame_buf_mut(frame_idx) };

                Ok(PageWriteGuard::new(
                    frame_idx,
                    frame_slice,
                    self,
                    write_latch,
                ))
            } else {
                let Some(write_latch) = _opt_write_latch else {
                    panic!("write latch missing when frame was not loaded");
                };
                let frame_slice = unsafe { &mut *self.frame_buf_mut(frame_idx) };

                let ppage_id = self
                    .disk_manager
                    .read_page(page_id, frame_slice)
                    .await
                    .map_err(|e| buffer::Error::StorageError(e))?;

                {
                    let mut meta = self.frame_meta.write().unwrap();
                    meta[frame_idx] = FrameMeta::Loaded {
                        lpage_id: page_id,
                        ppage_id,
                        dirty: false,
                    };
                }
                {
                    let mut ppage_map = self.frame_ppage_id_map.write().unwrap();
                    ppage_map.insert(ppage_id, frame_idx);
                }

                let write_guard = PageWriteGuard::new(frame_idx, frame_slice, self, write_latch);

                Ok(write_guard)
            }
        }
    }

    fn fetch_page_read(
        &self,
        page_id: aliases::LPageId,
    ) -> impl Future<Output = buffer::Result<Self::ReadGuard<'_>>> + Send + '_ {
        async move {
            let (frame_idx, is_loaded, _opt_write_latch) = loop {
                {
                    let map = self.frame_lpage_id_map.read().unwrap();
                    if let Some(&idx) = map.get(&page_id) {
                        self.incr_pin(idx);
                        break (idx, true, None);
                    }
                }

                let vacant_idx_opt = {
                    let mut vacant = self.vacant_frames.write().unwrap();
                    if let Some(&idx) = vacant.iter().next() {
                        vacant.remove(&idx);
                        Some(idx)
                    } else {
                        None
                    }
                };

                let vacant_idx = match vacant_idx_opt {
                    Some(idx) => idx,
                    None => {
                        self.evict().await?;
                        continue;
                    }
                };

                self.incr_pin(vacant_idx);

                let write_latch = self.frame_latches[vacant_idx].write().await;

                {
                    let mut map = self.frame_lpage_id_map_write();

                    if let Some(&idx) = map.get(&page_id) {
                        // Someone else loaded it while we were evicting/yielding.
                        self.decr_pin(vacant_idx);
                        self.vacant_frames_write().insert(vacant_idx);
                        // write_latch is automatically dropped here when it goes out of scope

                        self.incr_pin(idx);
                        break (idx, true, None);
                    }

                    // Safe to publish. We already hold the write_latch!
                    map.insert(page_id, vacant_idx);
                }

                // Pass the acquired write_latch out of the loop
                break (vacant_idx, false, Some(write_latch));
            };

            if is_loaded {
                let read_latch = self.frame_latches[frame_idx].read().await;
                let frame_slice = unsafe { &*self.frame_buf(frame_idx) };

                Ok(PageReadGuard::new(frame_idx, frame_slice, self, read_latch))
            } else {
                let Some(write_latch) = _opt_write_latch else {
                    panic!("write latch missing when frame was not loaded");
                };
                let frame_slice = unsafe { &mut *self.frame_buf_mut(frame_idx) };

                let ppage_id = self
                    .disk_manager
                    .read_page(page_id, frame_slice)
                    .await
                    .map_err(|e| buffer::Error::StorageError(e))?;

                {
                    let mut meta = self.frame_meta.write().unwrap();
                    meta[frame_idx] = FrameMeta::Loaded {
                        lpage_id: page_id,
                        ppage_id,
                        dirty: false,
                    };
                }
                {
                    let mut ppage_map = self.frame_ppage_id_map.write().unwrap();
                    ppage_map.insert(ppage_id, frame_idx);
                }

                let write_guard = PageWriteGuard::new(frame_idx, frame_slice, self, write_latch);

                Ok(write_guard.downgrade())
            }
        }
    }

    fn fetch_page_at_loc_write(
        &self,
        loc: aliases::PPageId,
    ) -> impl Future<Output = buffer::Result<Self::WriteGuard<'_>>> + Send + '_ {
        async move {
            let (frame_idx, is_loaded, _opt_write_latch) = loop {
                {
                    // 1. Search the PHYSICAL map instead of the logical map
                    let map = self.frame_ppage_id_map_read();
                    if let Some(&idx) = map.get(&loc) {
                        self.incr_pin(idx);
                        break (idx, true, None);
                    }
                }

                let vacant_idx_opt = {
                    let mut vacant = self.vacant_frames_write();
                    if let Some(&idx) = vacant.iter().next() {
                        vacant.remove(&idx);
                        Some(idx)
                    } else {
                        None
                    }
                };

                let vacant_idx = match vacant_idx_opt {
                    Some(idx) => idx,
                    None => {
                        self.evict().await?;
                        continue;
                    }
                };

                self.incr_pin(vacant_idx);

                let write_latch = self.frame_latches[vacant_idx].write().await;

                {
                    // 2. Double-check the PHYSICAL map
                    let mut map = self.frame_ppage_id_map_write();

                    if let Some(&idx) = map.get(&loc) {
                        self.decr_pin(vacant_idx);
                        self.vacant_frames_write().insert(vacant_idx);
                        self.incr_pin(idx);
                        break (idx, true, None);
                    }

                    // 3. Publish to the PHYSICAL map.
                    // Note: We cannot publish to the logical map yet because we
                    // don't know the LPageId until we read the disk!
                    map.insert(loc, vacant_idx);
                }

                break (vacant_idx, false, Some(write_latch));
            };

            if is_loaded {
                let write_latch = self.frame_latches[frame_idx].write().await;
                let frame_slice = unsafe { &mut *self.frame_buf_mut(frame_idx) };

                Ok(PageWriteGuard::new(
                    frame_idx,
                    frame_slice,
                    self,
                    write_latch,
                ))
            } else {
                let Some(write_latch) = _opt_write_latch else {
                    panic!("write latch missing when frame was not loaded");
                };
                let frame_slice = unsafe { &mut *self.frame_buf_mut(frame_idx) };

                // 4. Read from disk using the physical location.
                self.disk_manager
                    .read_page_at_loc(loc, frame_slice)
                    .await
                    .map_err(|e| buffer::Error::StorageError(e))?;

                let (lpage_id, frame_slice) = {
                    let overlay = overlays::UnknownPage::new(frame_slice);
                    (overlay.uber_header().unwrap().page_id, overlay.into_inner())
                };

                {
                    let mut meta = self.frame_meta_write();
                    meta[frame_idx] = FrameMeta::Loaded {
                        lpage_id, // We finally have the logical ID
                        ppage_id: loc,
                        dirty: false,
                    };
                }
                {
                    // 5. NOW we can safely insert it into the logical map
                    let mut lpage_map = self.frame_lpage_id_map_write();
                    lpage_map.insert(lpage_id, frame_idx);
                }

                let write_guard = PageWriteGuard::new(frame_idx, frame_slice, self, write_latch);

                Ok(write_guard)
            }
        }
    }

    fn fetch_page_at_loc_read(
        &self,
        loc: aliases::PPageId,
    ) -> impl Future<Output = buffer::Result<Self::ReadGuard<'_>>> + Send + '_ {
        async move {
            let (frame_idx, is_loaded, _opt_write_latch) = loop {
                {
                    // 1. Search the PHYSICAL map
                    let map = self.frame_ppage_id_map_read();
                    if let Some(&idx) = map.get(&loc) {
                        self.incr_pin(idx);
                        break (idx, true, None);
                    }
                }

                let vacant_idx_opt = {
                    let mut vacant = self.vacant_frames_write();
                    if let Some(&idx) = vacant.iter().next() {
                        vacant.remove(&idx);
                        Some(idx)
                    } else {
                        None
                    }
                };

                let vacant_idx = match vacant_idx_opt {
                    Some(idx) => idx,
                    None => {
                        self.evict().await?;
                        continue;
                    }
                };

                self.incr_pin(vacant_idx);

                let write_latch = self.frame_latches[vacant_idx].write().await;

                {
                    // 2. Double-check the PHYSICAL map
                    let mut map = self.frame_ppage_id_map_write();

                    if let Some(&idx) = map.get(&loc) {
                        self.decr_pin(vacant_idx);
                        self.vacant_frames_write().insert(vacant_idx);
                        self.incr_pin(idx);
                        break (idx, true, None);
                    }

                    // 3. Publish to the PHYSICAL map.
                    map.insert(loc, vacant_idx);
                }

                break (vacant_idx, false, Some(write_latch));
            };

            if is_loaded {
                // Already in memory - acquire read latch
                let read_latch = self.frame_latches[frame_idx].read().await;
                let frame_slice = unsafe { &*self.frame_buf(frame_idx) };

                Ok(PageReadGuard::new(frame_idx, frame_slice, self, read_latch))
            } else {
                // Need to load from disk - we hold write latch
                let Some(write_latch) = _opt_write_latch else {
                    panic!("write latch missing when frame was not loaded");
                };
                let frame_slice = unsafe { &mut *self.frame_buf_mut(frame_idx) };

                // 4. Read from disk using the physical location.
                self.disk_manager
                    .read_page_at_loc(loc, frame_slice)
                    .await
                    .map_err(|e| buffer::Error::StorageError(e))?;

                let (lpage_id, frame_slice) = {
                    let overlay = overlays::UnknownPage::new(frame_slice);
                    (overlay.uber_header().unwrap().page_id, overlay.into_inner())
                };

                {
                    let mut meta = self.frame_meta_write();
                    meta[frame_idx] = FrameMeta::Loaded {
                        lpage_id,
                        ppage_id: loc,
                        dirty: false,
                    };
                }
                {
                    // 5. NOW we can safely insert it into the logical map
                    let mut lpage_map = self.frame_lpage_id_map_write();
                    lpage_map.insert(lpage_id, frame_idx);
                }

                // Downgrade write latch to read latch
                let write_guard = PageWriteGuard::new(frame_idx, frame_slice, self, write_latch);
                Ok(write_guard.downgrade())
            }
        }
    }

    fn new_page(
        &self,
        file_id: aliases::FileId,
    ) -> impl Future<Output = buffer::Result<Self::WriteGuard<'_>>> + Send + '_ {
        async move {
            // 1. Allocate logical and physical page IDs via disk manager
            let (lpage_id, ppage_id) = self
                .disk_manager
                .new_page(file_id)
                .await
                .map_err(|e| buffer::Error::StorageError(e))?;

            // 2. Get a vacant frame (evict if needed)
            let frame_idx = loop {
                let vacant_idx_opt = {
                    let mut vacant = self.vacant_frames_write();
                    if let Some(&idx) = vacant.iter().next() {
                        vacant.remove(&idx);
                        Some(idx)
                    } else {
                        None
                    }
                };

                match vacant_idx_opt {
                    Some(idx) => break idx,
                    None => {
                        self.evict().await?;
                        continue;
                    }
                }
            };

            // 3. Pin the frame and acquire write latch
            self.incr_pin(frame_idx);
            let write_latch = self.frame_latches[frame_idx].write().await;

            // 4. Initialize frame buffer to zeros (new page starts empty)
            let frame_slice = unsafe { &mut *self.frame_buf_mut(frame_idx) };
            frame_slice.fill(0);

            // 5. Set up metadata
            {
                let mut meta = self.frame_meta_write();
                meta[frame_idx] = FrameMeta::Loaded {
                    lpage_id,
                    ppage_id,
                    dirty: false, // Caller will mark dirty after writing content
                };
            }

            // 6. Add to maps
            {
                let mut lpage_map = self.frame_lpage_id_map_write();
                lpage_map.insert(lpage_id, frame_idx);
            }
            {
                let mut ppage_map = self.frame_ppage_id_map_write();
                ppage_map.insert(ppage_id, frame_idx);
            }

            // 7. Return write guard
            Ok(PageWriteGuard::new(
                frame_idx,
                frame_slice,
                self,
                write_latch,
            ))
        }
    }

    fn flush_all_dirty(&self) -> impl Future<Output = buffer::Result<()>> + Send + '_ {
        async move {
            // Collect dirty frames to avoid holding the meta lock during I/O
            let dirty_frames: Vec<(usize, aliases::PPageId)> = {
                let meta = self.frame_meta_read();
                meta.iter()
                    .enumerate()
                    .filter_map(|(idx, m)| match m {
                        FrameMeta::Loaded {
                            ppage_id, dirty, ..
                        } if *dirty => Some((idx, *ppage_id)),
                        _ => None,
                    })
                    .collect()
            };

            // Flush each dirty frame
            for (frame_idx, ppage_id) in dirty_frames {
                // Acquire read latch - sufficient for reading frame data
                let _read_latch = self.frame_latches[frame_idx].read().await;

                // Double-check the frame is still dirty (might have been evicted and reloaded)
                let still_dirty = {
                    let meta = self.frame_meta_read();
                    matches!(
                        meta[frame_idx],
                        FrameMeta::Loaded {
                            dirty: true,
                            ppage_id: pid,
                            ..
                        } if pid == ppage_id
                    )
                };

                if !still_dirty {
                    continue;
                }

                // Read frame data (safe under read latch)
                let frame_slice = unsafe { &*self.frame_buf(frame_idx) };

                // Write to disk
                self.disk_manager
                    .write_page_at_loc(ppage_id, frame_slice)
                    .await
                    .map_err(|e| buffer::Error::StorageError(e))?;

                // Mark as clean
                {
                    let mut meta = self.frame_meta_write();
                    if let FrameMeta::Loaded { dirty, .. } = &mut meta[frame_idx] {
                        *dirty = false;
                    }
                }
            }

            Ok(())
        }
    }
}
