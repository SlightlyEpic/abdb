//! Directory B-Tree Leaf Page Overlay
//!
//! Terminal nodes of the directory B-Tree that store mappings from logical
//! PageIds to physical locations (page + slot).
//!
//! # Binary Layout (4096 bytes)
//!
//! ```text
//! Offset  | Size | Field        | Description
//! --------|------|--------------|---------------------------
//! 0-15    | 16   | uber_header  | UberPageHeader
//! 16-39   | 24   | leaf_header  | DirectoryLeafHeader
//! 40-4095 | 4056 | entries[]    | DirectoryLeafEntry array
//! ```
//!
//! # Thread Safety
//!
//! All methods are synchronous and pure (no I/O, no blocking).
//! Thread safety is enforced at the BufferPool level via latches:
//! - Read methods can be called under shared or exclusive latch
//! - Write methods require exclusive latch (caller's responsibility)
//!
//! # Async Safety
//!
//! All methods are async-safe: no await points, pure memory operations.
//! Safe to call while holding async locks.

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

use crate::{
    common::{
        aliases::{FileId, LPageId, PPageId},
        constants::PAGE_BUF_SIZE,
    },
    page::{PageType, UberPageHeader, header::UBER_HEADER_SIZE},
};

use super::error::OverlayError;

// ============================================================================
// CONSTANTS
// ============================================================================

/// Size of each DirectoryLeafEntry in bytes.
const ENTRY_SIZE: usize = size_of::<DirectoryLeafEntry>();

/// Offset where entries begin (after UberPageHeader + LeafHeader).
const ENTRIES_OFFSET: usize = UBER_HEADER_SIZE + size_of::<DirectoryLeafHeader>();

/// Maximum number of entries per leaf page.
pub const MAX_ENTRIES: u16 = ((PAGE_BUF_SIZE - ENTRIES_OFFSET) / ENTRY_SIZE) as u16;

/// Safety threshold for latch crabbing.
const SAFE_INSERT_THRESHOLD: u16 = MAX_ENTRIES - 1;

/// Threshold below which a page should be considered for merging.
const MERGE_THRESHOLD: u16 = MAX_ENTRIES / 2;

// ============================================================================
// HEADER STRUCTURE
// ============================================================================

/// Type-specific header for directory leaf pages.
///
/// Immediately follows the UberPageHeader in the page buffer.
/// All fields are <= u32 to avoid alignment padding within `#[repr(C)]`.
///
/// # Layout (24 bytes, C repr — no internal padding)
///
/// ```text
/// Bytes 0-1:   num_entries   (u16)
/// Bytes 2-3:   flags         (u16)
/// Bytes 4-7:   next_page     (u32) - Right sibling
/// Bytes 8-11:  prev_page     (u32) - Left sibling
/// Bytes 12-15: reserved_lo   (u32) - Future MVCC (low)
/// Bytes 16-19: reserved_hi   (u32) - Future MVCC (high)
/// Bytes 20-23: _padding      (u32)
/// ```
#[repr(C)]
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable)]
struct DirectoryLeafHeader {
    num_entries: u16,
    flags: u16,
    next_page: LPageId,
    prev_page: LPageId,
    reserved_lo: u32,
    reserved_hi: u32,
    _padding: u32,
}

const _: () = assert!(size_of::<DirectoryLeafHeader>() == 24);

// ============================================================================
// ENTRY STRUCTURE
// ============================================================================

/// A single entry in a directory leaf page.
///
/// Maps a logical PageId (search_key) to a physical location (file_id + page_number).
/// The physical byte offset is computed as `page_number * PAGE_SIZE`.
///
/// # Layout (16 bytes, C repr)
///
/// ```text
/// Bytes 0-7:   search_key   (u64) - LPageId stored as u64 for B-tree search
/// Bytes 8-11:  file_id      (u32) - FileId of the file containing the page
/// Bytes 12-15: page_number  (u32) - Page number within the file (offset = page_number * PAGE_SIZE)
/// ```
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
pub struct DirectoryLeafEntry {
    pub search_key: u64,
    pub file_id: FileId,
    pub page_number: u32,
}

const _: () = assert!(size_of::<DirectoryLeafEntry>() == 16);

impl DirectoryLeafEntry {
    #[inline]
    pub fn new(logical_page_id: LPageId, file_id: FileId, page_number: u32) -> Self {
        Self {
            search_key: logical_page_id as u64,
            file_id,
            page_number,
        }
    }

    /// Construct from a logical page ID and physical page ID.
    #[inline]
    pub fn from_ppage_id(logical_page_id: LPageId, ppage_id: PPageId) -> Self {
        let page_number = (ppage_id.offset / PAGE_BUF_SIZE as u64) as u32;
        Self {
            search_key: logical_page_id as u64,
            file_id: ppage_id.file,
            page_number,
        }
    }

    /// Convert to a physical page ID.
    #[inline]
    pub fn to_ppage_id(&self) -> PPageId {
        PPageId {
            file: self.file_id,
            offset: self.page_number as u64 * PAGE_BUF_SIZE as u64,
        }
    }

    /// Get the logical page ID (search key as LPageId).
    #[inline]
    pub fn logical_page_id(&self) -> LPageId {
        self.search_key as LPageId
    }
}

// ============================================================================
// LEAF PAGE OVERLAY
// ============================================================================

/// Directory B-Tree leaf page overlay.
///
/// Wraps a page buffer and provides typed access to leaf page structure.
/// Uses zerocopy `read_from_prefix`/`write_to_prefix` for all field access
/// to avoid alignment requirements on the underlying buffer.
pub struct DirectoryLeafPage<T> {
    data: T,
}

// ============================================================================
// READ-ONLY METHODS
// ============================================================================

impl<T> DirectoryLeafPage<T>
where
    T: AsRef<[u8]>,
{
    /// Wrap a buffer as a directory leaf page.
    ///
    /// # Panics
    ///
    /// Panics if buffer size is not exactly PAGE_BUF_SIZE (4096 bytes).
    #[inline]
    pub fn new(data: T) -> Self {
        let len = data.as_ref().len();
        assert!(
            len == PAGE_BUF_SIZE,
            "DirectoryLeafPage::new called with buffer of size {} (expected {})",
            len,
            PAGE_BUF_SIZE
        );
        Self { data }
    }

    /// Wrap a buffer and validate it's a directory leaf page.
    ///
    /// # Errors
    ///
    /// Returns `OverlayError::TypeMismatch` if the page type is not DirectoryLeaf.
    pub fn from_buffer(data: T) -> Result<Self, OverlayError> {
        let page = Self::new(data);
        page.validate_type()?;
        Ok(page)
    }

    fn validate_type(&self) -> Result<(), OverlayError> {
        let uber = self.uber_header();
        let page_type = PageType::try_from(uber.page_type_id)
            .map_err(|_| OverlayError::InvalidPageType(uber.page_type_id))?;

        if page_type != PageType::DirectoryLeaf {
            return Err(OverlayError::TypeMismatch {
                expected: PageType::DirectoryLeaf,
                found: page_type,
            });
        }
        Ok(())
    }

    #[inline]
    pub fn as_buffer(&self) -> &[u8] {
        self.data.as_ref()
    }

    // -- UberPageHeader (bytes 0-15) -----------------------------------------

    /// Read the UberPageHeader via zerocopy (copy-based, no alignment needed).
    #[inline]
    fn uber_header(&self) -> UberPageHeader {
        UberPageHeader::read_from_prefix(self.data.as_ref())
            .expect("UberPageHeader must fit in page buffer")
            .0
    }

    #[inline]
    pub fn page_lsn(&self) -> u64 {
        self.uber_header().page_lsn
    }

    #[inline]
    pub fn page_id(&self) -> LPageId {
        self.uber_header().page_id
    }

    #[inline]
    pub fn page_type_byte(&self) -> u8 {
        self.uber_header().page_type_id
    }

    // -- Leaf Header (bytes 16-39) -------------------------------------------

    /// Read the leaf-specific header via zerocopy (copy-based).
    #[inline]
    fn leaf_header(&self) -> DirectoryLeafHeader {
        DirectoryLeafHeader::read_from_prefix(&self.data.as_ref()[UBER_HEADER_SIZE..])
            .expect("DirectoryLeafHeader must fit after UberPageHeader")
            .0
    }

    #[inline]
    pub fn num_entries(&self) -> u16 {
        self.leaf_header().num_entries
    }

    #[inline]
    pub fn flags(&self) -> u16 {
        self.leaf_header().flags
    }

    #[inline]
    pub fn next_page(&self) -> LPageId {
        self.leaf_header().next_page
    }

    #[inline]
    pub fn prev_page(&self) -> LPageId {
        self.leaf_header().prev_page
    }

    #[inline]
    pub fn reserved(&self) -> u64 {
        let h = self.leaf_header();
        (h.reserved_hi as u64) << 32 | h.reserved_lo as u64
    }

    // -- Entry accessors -----------------------------------------------------

    /// Read an entry at the given index via zerocopy `read_from_prefix`.
    pub fn entry(&self, index: u16) -> Result<DirectoryLeafEntry, OverlayError> {
        let num = self.num_entries();
        if index >= num {
            return Err(OverlayError::IndexOutOfBounds { index, max: num });
        }

        let offset = ENTRIES_OFFSET + (index as usize * ENTRY_SIZE);
        Ok(
            DirectoryLeafEntry::read_from_prefix(&self.data.as_ref()[offset..])
                .expect("Entry must fit at calculated offset")
                .0,
        )
    }

    /// Binary search for a key position.
    ///
    /// Returns `(found, index)` — exact match or insertion point.
    pub fn find_slot(&self, target_key: u64) -> (bool, u16) {
        let num_entries = self.num_entries();
        if num_entries == 0 {
            return (false, 0);
        }

        let mut left: u16 = 0;
        let mut right: u16 = num_entries;

        while left < right {
            let mid = left + (right - left) / 2;
            let mid_key = self.entry(mid).unwrap().search_key;

            match mid_key.cmp(&target_key) {
                std::cmp::Ordering::Equal => return (true, mid),
                std::cmp::Ordering::Less => left = mid + 1,
                std::cmp::Ordering::Greater => right = mid,
            }
        }

        (false, left)
    }

    /// Look up a key. Returns `Some(PPageId)` if found.
    pub fn lookup(&self, key: u64) -> Option<PPageId> {
        let (found, pos) = self.find_slot(key);
        if found {
            Some(self.entry(pos).unwrap().to_ppage_id())
        } else {
            None
        }
    }

    // -- Safety checks -------------------------------------------------------

    #[inline]
    pub fn is_safe_for_insert(&self) -> bool {
        self.num_entries() < SAFE_INSERT_THRESHOLD
    }

    #[inline]
    pub fn needs_split(&self) -> bool {
        self.num_entries() >= MAX_ENTRIES
    }

    #[inline]
    pub fn can_merge(&self) -> bool {
        self.num_entries() < MERGE_THRESHOLD
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.num_entries() == 0
    }

    #[inline]
    pub fn capacity(&self) -> u16 {
        MAX_ENTRIES
    }

    pub fn into_inner(self) -> T {
        self.data
    }
}

// ============================================================================
// MUTABLE METHODS
// ============================================================================

impl<T> DirectoryLeafPage<T>
where
    T: AsRef<[u8]> + AsMut<[u8]>,
{
    /// Initialize a new directory leaf page.
    ///
    /// Uses zerocopy `write_to_prefix` for type-safe header initialization.
    pub fn init(mut data: T, page_id: LPageId) -> Self {
        let buffer = data.as_mut();
        buffer.fill(0);

        // Write UberPageHeader via zerocopy (copy-based, alignment-safe)
        let uber = Self::read_uber_header(buffer);
        let mut uber = uber;
        uber.page_lsn = 0;
        uber.page_id = page_id;
        uber.page_type_id = PageType::DirectoryLeaf as u8;
        uber.write_to_prefix(buffer)
            .expect("UberPageHeader must fit in page buffer");

        // LeafHeader is already zeroed which is correct defaults

        Self { data }
    }

    /// Helper: read UberPageHeader from raw buffer (copy-based).
    fn read_uber_header(buf: &[u8]) -> UberPageHeader {
        UberPageHeader::read_from_prefix(buf)
            .expect("UberPageHeader must fit in page buffer")
            .0
    }

    #[inline]
    pub fn as_buffer_mut(&mut self) -> &mut [u8] {
        self.data.as_mut()
    }

    // -- Mutable header accessors (read-modify-write via zerocopy) -----------

    /// Read-modify-write the leaf header.
    #[inline]
    fn update_leaf_header(&mut self, f: impl FnOnce(&mut DirectoryLeafHeader)) {
        let mut header =
            DirectoryLeafHeader::read_from_prefix(&self.data.as_ref()[UBER_HEADER_SIZE..])
                .expect("DirectoryLeafHeader must fit")
                .0;

        f(&mut header);

        header
            .write_to_prefix(&mut self.data.as_mut()[UBER_HEADER_SIZE..])
            .expect("DirectoryLeafHeader must fit");
    }

    #[inline]
    pub fn set_page_lsn(&mut self, lsn: u64) {
        let mut uber = Self::read_uber_header(self.data.as_ref());
        uber.page_lsn = lsn;
        uber.write_to_prefix(self.data.as_mut())
            .expect("UberPageHeader must fit");
    }

    #[inline]
    fn set_num_entries(&mut self, count: u16) {
        self.update_leaf_header(|h| h.num_entries = count);
    }

    #[inline]
    pub fn set_flags(&mut self, flags: u16) {
        self.update_leaf_header(|h| h.flags = flags);
    }

    #[inline]
    pub fn set_next_page(&mut self, page_id: LPageId) {
        self.update_leaf_header(|h| h.next_page = page_id);
    }

    #[inline]
    pub fn set_prev_page(&mut self, page_id: LPageId) {
        self.update_leaf_header(|h| h.prev_page = page_id);
    }

    #[inline]
    pub fn set_reserved(&mut self, value: u64) {
        self.update_leaf_header(|h| {
            h.reserved_lo = value as u32;
            h.reserved_hi = (value >> 32) as u32;
        });
    }

    // -- Entry modification via zerocopy -------------------------------------

    /// Write an entry via `write_to_prefix`.
    fn set_entry(&mut self, index: u16, entry: DirectoryLeafEntry) {
        assert!(
            index < MAX_ENTRIES,
            "Entry index {} exceeds capacity {}",
            index,
            MAX_ENTRIES
        );

        let offset = ENTRIES_OFFSET + (index as usize * ENTRY_SIZE);
        entry
            .write_to_prefix(&mut self.data.as_mut()[offset..])
            .expect("Entry must fit at calculated offset");
    }

    /// Insert a new entry maintaining sorted order.
    ///
    /// Uses binary search + `copy_within` for shifting + zerocopy for writing.
    pub fn insert_entry(
        &mut self,
        logical_page_id: LPageId,
        ppage_id: PPageId,
    ) -> Result<(), OverlayError> {
        let num_entries = self.num_entries();

        if num_entries >= MAX_ENTRIES {
            return Err(OverlayError::PageFull {
                capacity: MAX_ENTRIES as usize,
                attempted: num_entries as usize + 1,
            });
        }

        let search_key = logical_page_id as u64;
        let (found, insert_pos) = self.find_slot(search_key);
        if found {
            return Err(OverlayError::DuplicateKey { key: search_key });
        }

        // Shift entries right (copy_within handles overlapping regions)
        if insert_pos < num_entries {
            let src_start = ENTRIES_OFFSET + (insert_pos as usize * ENTRY_SIZE);
            let dst_start = ENTRIES_OFFSET + ((insert_pos + 1) as usize * ENTRY_SIZE);
            let count = (num_entries - insert_pos) as usize * ENTRY_SIZE;
            self.data
                .as_mut()
                .copy_within(src_start..(src_start + count), dst_start);
        }

        self.set_entry(
            insert_pos,
            DirectoryLeafEntry::from_ppage_id(logical_page_id, ppage_id),
        );
        self.set_num_entries(num_entries + 1);

        Ok(())
    }

    /// Delete an entry at the given index.
    pub fn delete_entry(&mut self, index: u16) -> Result<(), OverlayError> {
        let num_entries = self.num_entries();
        if index >= num_entries {
            return Err(OverlayError::IndexOutOfBounds {
                index,
                max: num_entries,
            });
        }

        if index < num_entries - 1 {
            let src_start = ENTRIES_OFFSET + ((index + 1) as usize * ENTRY_SIZE);
            let dst_start = ENTRIES_OFFSET + (index as usize * ENTRY_SIZE);
            let count = (num_entries - 1 - index) as usize * ENTRY_SIZE;
            self.data
                .as_mut()
                .copy_within(src_start..(src_start + count), dst_start);
        }

        self.set_num_entries(num_entries - 1);
        Ok(())
    }

    /// Delete an entry by key. Returns `Ok(true)` if found and deleted.
    pub fn delete_by_key(&mut self, key: u64) -> Result<bool, OverlayError> {
        let (found, pos) = self.find_slot(key);
        if found {
            self.delete_entry(pos)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Update the physical location for an existing key.
    /// Returns `Ok(true)` if found and updated.
    pub fn update_entry(
        &mut self,
        logical_page_id: LPageId,
        new_ppage_id: PPageId,
    ) -> Result<bool, OverlayError> {
        let search_key = logical_page_id as u64;
        let (found, pos) = self.find_slot(search_key);
        if found {
            self.set_entry(
                pos,
                DirectoryLeafEntry::from_ppage_id(logical_page_id, new_ppage_id),
            );
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uber_header_size() {
        assert_eq!(
            UBER_HEADER_SIZE, 16,
            "UberPageHeader size changed! Expected 16, got {}",
            UBER_HEADER_SIZE
        );
    }

    #[test]
    fn test_leaf_header_offsets() {
        let buffer = [0u8; PAGE_BUF_SIZE];
        let mut page = DirectoryLeafPage::init(buffer, 42);

        page.set_num_entries(0x1234);
        assert_eq!(page.as_buffer()[16], 0x34);
        assert_eq!(page.as_buffer()[17], 0x12);

        page.set_flags(0xABCD);
        assert_eq!(page.as_buffer()[18], 0xCD);
        assert_eq!(page.as_buffer()[19], 0xAB);

        page.set_next_page(0x11223344);
        assert_eq!(&page.as_buffer()[20..24], &[0x44, 0x33, 0x22, 0x11]);

        page.set_prev_page(0x55667788);
        assert_eq!(&page.as_buffer()[24..28], &[0x88, 0x77, 0x66, 0x55]);
    }

    #[test]
    fn test_capacity_constants() {
        assert_eq!(MAX_ENTRIES, 253);
        assert_eq!(SAFE_INSERT_THRESHOLD, 252);
        assert_eq!(MERGE_THRESHOLD, 126);
    }

    #[test]
    fn test_entry_structure() {
        let entry = DirectoryLeafEntry::new(100, 5, 3);
        assert_eq!(entry.search_key, 100);
        assert_eq!(entry.file_id, 5);
        assert_eq!(entry.page_number, 3);
        let ppage = entry.to_ppage_id();
        assert_eq!(ppage.file, 5);
        assert_eq!(ppage.offset, 3 * PAGE_BUF_SIZE as u64);
    }

    #[test]
    fn test_init() {
        let buffer = [0u8; PAGE_BUF_SIZE];
        let page = DirectoryLeafPage::init(buffer, 42);

        assert_eq!(page.page_id(), 42);
        assert_eq!(page.page_type_byte(), PageType::DirectoryLeaf as u8);
        assert_eq!(page.num_entries(), 0);
        assert_eq!(page.flags(), 0);
        assert_eq!(page.next_page(), 0);
        assert_eq!(page.prev_page(), 0);
        assert!(page.is_empty());
        assert!(!page.needs_split());
        assert!(page.is_safe_for_insert());
    }

    #[test]
    fn test_binary_search() {
        let buffer = [0u8; PAGE_BUF_SIZE];
        let mut page = DirectoryLeafPage::init(buffer, 1);

        let ppage = |file, page_num| PPageId {
            file,
            offset: page_num * PAGE_BUF_SIZE as u64,
        };

        page.insert_entry(300, ppage(1, 0)).unwrap();
        page.insert_entry(100, ppage(2, 0)).unwrap();
        page.insert_entry(200, ppage(3, 0)).unwrap();
        page.insert_entry(500, ppage(4, 0)).unwrap();
        page.insert_entry(400, ppage(5, 0)).unwrap();

        assert_eq!(page.entry(0).unwrap().search_key, 100);
        assert_eq!(page.entry(1).unwrap().search_key, 200);
        assert_eq!(page.entry(2).unwrap().search_key, 300);
        assert_eq!(page.entry(3).unwrap().search_key, 400);
        assert_eq!(page.entry(4).unwrap().search_key, 500);

        assert_eq!(page.find_slot(100), (true, 0));
        assert_eq!(page.find_slot(200), (true, 1));
        assert_eq!(page.find_slot(300), (true, 2));
        assert_eq!(page.find_slot(400), (true, 3));
        assert_eq!(page.find_slot(500), (true, 4));

        assert_eq!(page.find_slot(50), (false, 0));
        assert_eq!(page.find_slot(150), (false, 1));
        assert_eq!(page.find_slot(250), (false, 2));
        assert_eq!(page.find_slot(350), (false, 3));
        assert_eq!(page.find_slot(450), (false, 4));
        assert_eq!(page.find_slot(600), (false, 5));
    }

    #[test]
    fn test_lookup() {
        let buffer = [0u8; PAGE_BUF_SIZE];
        let mut page = DirectoryLeafPage::init(buffer, 1);

        let ppage = |file, page_num| PPageId {
            file,
            offset: page_num * PAGE_BUF_SIZE as u64,
        };

        page.insert_entry(100, ppage(5, 3)).unwrap();
        page.insert_entry(200, ppage(10, 7)).unwrap();

        assert_eq!(page.lookup(100), Some(ppage(5, 3)));
        assert_eq!(page.lookup(200), Some(ppage(10, 7)));
        assert_eq!(page.lookup(150), None);
    }

    #[test]
    fn test_duplicate_key() {
        let buffer = [0u8; PAGE_BUF_SIZE];
        let mut page = DirectoryLeafPage::init(buffer, 1);

        let ppage = |file, page_num| PPageId {
            file,
            offset: page_num * PAGE_BUF_SIZE as u64,
        };

        page.insert_entry(100, ppage(1, 0)).unwrap();

        let result = page.insert_entry(100, ppage(2, 0));
        assert!(matches!(
            result,
            Err(OverlayError::DuplicateKey { key: 100 })
        ));
    }

    #[test]
    fn test_capacity_enforcement() {
        let buffer = [0u8; PAGE_BUF_SIZE];
        let mut page = DirectoryLeafPage::init(buffer, 1);

        let ppage = PPageId { file: 0, offset: 0 };

        for i in 0..MAX_ENTRIES {
            page.insert_entry(i as u32, ppage).unwrap();
        }

        assert!(page.needs_split());
        assert!(!page.is_safe_for_insert());

        let result = page.insert_entry(999, ppage);
        assert!(matches!(result, Err(OverlayError::PageFull { .. })));
    }

    #[test]
    fn test_delete_entry() {
        let buffer = [0u8; PAGE_BUF_SIZE];
        let mut page = DirectoryLeafPage::init(buffer, 1);

        let ppage = |file| PPageId { file, offset: 0 };

        page.insert_entry(100, ppage(1)).unwrap();
        page.insert_entry(200, ppage(2)).unwrap();
        page.insert_entry(300, ppage(3)).unwrap();

        page.delete_entry(1).unwrap();

        assert_eq!(page.num_entries(), 2);
        assert_eq!(page.entry(0).unwrap().search_key, 100);
        assert_eq!(page.entry(1).unwrap().search_key, 300);
    }

    #[test]
    fn test_delete_by_key() {
        let buffer = [0u8; PAGE_BUF_SIZE];
        let mut page = DirectoryLeafPage::init(buffer, 1);

        let ppage = |file| PPageId { file, offset: 0 };

        page.insert_entry(100, ppage(1)).unwrap();
        page.insert_entry(200, ppage(2)).unwrap();

        assert!(page.delete_by_key(100).unwrap());
        assert!(!page.delete_by_key(100).unwrap());
        assert_eq!(page.num_entries(), 1);
        assert_eq!(page.entry(0).unwrap().search_key, 200);
    }

    #[test]
    fn test_update_entry() {
        let buffer = [0u8; PAGE_BUF_SIZE];
        let mut page = DirectoryLeafPage::init(buffer, 1);

        let ppage = |file, page_num| PPageId {
            file,
            offset: page_num * PAGE_BUF_SIZE as u64,
        };

        page.insert_entry(100, ppage(1, 0)).unwrap();
        assert!(page.update_entry(100, ppage(99, 88)).unwrap());
        assert_eq!(page.lookup(100), Some(ppage(99, 88)));
        assert!(!page.update_entry(999, ppage(1, 1)).unwrap());
    }

    #[test]
    fn test_from_buffer_validation() {
        let buffer = [0u8; PAGE_BUF_SIZE];
        let page = DirectoryLeafPage::init(buffer, 1);

        let valid_buffer: [u8; PAGE_BUF_SIZE] = page.as_buffer().try_into().unwrap();
        let result = DirectoryLeafPage::from_buffer(valid_buffer);
        assert!(result.is_ok());

        let mut bad_buffer = [0u8; PAGE_BUF_SIZE];
        bad_buffer[12] = PageType::DirectoryInner as u8;
        let result = DirectoryLeafPage::from_buffer(bad_buffer);
        assert!(matches!(result, Err(OverlayError::TypeMismatch { .. })));
    }

    #[test]
    fn test_safety_threshold() {
        let buffer = [0u8; PAGE_BUF_SIZE];
        let mut page = DirectoryLeafPage::init(buffer, 1);

        let ppage = PPageId { file: 0, offset: 0 };

        for i in 0..(SAFE_INSERT_THRESHOLD - 1) {
            page.insert_entry(i as u32, ppage).unwrap();
        }
        assert!(page.is_safe_for_insert());

        page.insert_entry(SAFE_INSERT_THRESHOLD as u32, ppage)
            .unwrap();
        assert!(!page.is_safe_for_insert());
    }
}
