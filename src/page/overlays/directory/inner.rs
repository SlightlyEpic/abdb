//! Directory B-Tree Inner Page Overlay
//!
//! Routing layer for the directory B-Tree that guides searches to appropriate
//! child pages. Inner nodes store separator keys and child pointers.
//!
//! # Binary Layout (4096 bytes)
//!
//! Offset  | Size | Field          | Description
//! --------|------|----------------|---------------------------
//! 0-15    | 16   | uber_header    | UberPageHeader
//! 16-23   | 8    | inner_header   | DirectoryInnerHeader
//! 24-4095 | 4072 | entries[]      | DirectoryInnerEntry array
//!
//! # B-Tree Routing Semantics
//!
//! An inner node with `n` separator keys has `n + 1` children:
//!
//! leftmost_child | sep_key[0] | child[0] | sep_key[1] | child[1] | ...
//!       ↓              ↓           ↓           ↓           ↓
//!   keys < k0      k0 ≤ k < k1   keys ≥ k1

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

use crate::{
    common::{aliases::LPageId, constants::PAGE_BUF_SIZE},
    page::{header::UBER_HEADER_SIZE, PageType, UberPageHeader},
};

use super::error::OverlayError;

// ============================================================================
// CONSTANTS
// ============================================================================

/// Size of each DirectoryInnerEntry in bytes.
const ENTRY_SIZE: usize = size_of::<DirectoryInnerEntry>();

/// Offset where entries begin (after UberPageHeader + InnerHeader).
const ENTRIES_OFFSET: usize = UBER_HEADER_SIZE + size_of::<DirectoryInnerHeader>();

/// Maximum number of separator keys per inner node.
pub const MAX_KEYS: u16 = ((PAGE_BUF_SIZE - ENTRIES_OFFSET) / ENTRY_SIZE) as u16;

/// Safety threshold for latch crabbing.
const SAFE_INSERT_THRESHOLD: u16 = MAX_KEYS - 1;

/// Threshold below which a page should be considered for merging.
const MERGE_THRESHOLD: u16 = MAX_KEYS / 2;

// ============================================================================
// HEADER STRUCTURE
// ============================================================================

/// Type-specific header for directory inner pages.
///
/// Immediately follows the UberPageHeader in the page buffer.
/// All fields are <= u32 to avoid alignment padding.
///
/// # Layout (8 bytes, C repr — no internal padding)
///
/// Bytes 0-1: num_keys       (u16)
/// Bytes 2-3: flags          (u16)
/// Bytes 4-7: leftmost_child (u32)
#[repr(C)]
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable)]
struct DirectoryInnerHeader {
    num_keys: u16,
    flags: u16,
    leftmost_child: LPageId,
}

const _: () = assert!(size_of::<DirectoryInnerHeader>() == 8);

// ============================================================================
// ENTRY STRUCTURE
// ============================================================================

/// A single entry in a directory inner page.
///
/// # Layout (16 bytes, C repr)
///
/// Bytes 0-7:   separator_key  (u64)
/// Bytes 8-11:  right_child    (u32)
/// Bytes 12-15: _padding       (u32)
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
pub struct DirectoryInnerEntry {
    pub separator_key: u64,
    pub right_child: LPageId,
    _padding: u32,
}

const _: () = assert!(size_of::<DirectoryInnerEntry>() == 16);

impl DirectoryInnerEntry {
    #[inline]
    pub fn new(separator_key: u64, right_child: LPageId) -> Self {
        Self {
            separator_key,
            right_child,
            _padding: 0,
        }
    }
}

// ============================================================================
// INNER PAGE OVERLAY
// ============================================================================

/// Directory B-Tree inner page overlay.
///
/// Uses zerocopy `read_from_prefix`/`write_to_prefix` for all access,
/// so the underlying buffer has no alignment requirements.
pub struct DirectoryInnerPage<T> {
    data: T,
}

// ============================================================================
// READ-ONLY METHODS
// ============================================================================

impl<T> DirectoryInnerPage<T>
where
    T: AsRef<[u8]>,
{
    #[inline]
    pub fn new(data: T) -> Self {
        let len = data.as_ref().len();
        assert!(
            len == PAGE_BUF_SIZE,
            "DirectoryInnerPage::new called with buffer of size {} (expected {})",
            len,
            PAGE_BUF_SIZE
        );
        Self { data }
    }

    pub fn from_buffer(data: T) -> Result<Self, OverlayError> {
        let page = Self::new(data);
        match page.validate_type() {
            Ok(_) => Ok(page),
            Err(e) => Err(e),
        }
    }

    fn validate_type(&self) -> Result<(), OverlayError> {
        let uber = self.uber_header();
        let page_type = PageType::try_from(uber.page_type_id)
            .map_err(|_| OverlayError::InvalidPageType(uber.page_type_id))?;

        if page_type != PageType::DirectoryInner {
            return Err(OverlayError::TypeMismatch {
                expected: PageType::DirectoryInner,
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

    /// Read UberPageHeader via zerocopy (copy-based, no alignment needed).
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

    // -- Inner Header (bytes 16-23) ------------------------------------------

    /// Read inner-specific header via zerocopy (copy-based).
    #[inline]
    fn inner_header(&self) -> DirectoryInnerHeader {
        DirectoryInnerHeader::read_from_prefix(&self.data.as_ref()[UBER_HEADER_SIZE..])
            .expect("DirectoryInnerHeader must fit after UberPageHeader")
            .0
    }

    #[inline]
    pub fn num_keys(&self) -> u16 {
        self.inner_header().num_keys
    }

    #[inline]
    pub fn flags(&self) -> u16 {
        self.inner_header().flags
    }

    #[inline]
    pub fn leftmost_child(&self) -> LPageId {
        self.inner_header().leftmost_child
    }

    // -- Entry accessors -----------------------------------------------------

    /// Read an entry via zerocopy `read_from_prefix`.
    pub fn entry(&self, index: u16) -> Result<DirectoryInnerEntry, OverlayError> {
        let num = self.num_keys();
        if index >= num {
            return Err(OverlayError::IndexOutOfBounds { index, max: num });
        }

        let offset = ENTRIES_OFFSET + (index as usize * ENTRY_SIZE);
        Ok(
            DirectoryInnerEntry::read_from_prefix(&self.data.as_ref()[offset..])
                .expect("Entry must fit at calculated offset")
                .0,
        )
    }

    /// Core B-Tree routing: find which child to descend into for `search_key`.
    pub fn find_child(&self, search_key: u64) -> LPageId {
        let num_keys = self.num_keys();
        if num_keys == 0 {
            return self.leftmost_child();
        }

        let first_sep = self.entry(0).unwrap().separator_key;
        if search_key < first_sep {
            return self.leftmost_child();
        }

        // Binary search for largest separator <= search_key
        let mut left: u16 = 0;
        let mut right: u16 = num_keys;
        let mut result: u16 = 0;

        while left < right {
            let mid = left + (right - left) / 2;
            let mid_sep = self.entry(mid).unwrap().separator_key;

            match search_key.cmp(&mid_sep) {
                std::cmp::Ordering::Greater | std::cmp::Ordering::Equal => {
                    result = mid;
                    left = mid + 1;
                }
                std::cmp::Ordering::Less => {
                    right = mid;
                }
            }
        }

        self.entry(result).unwrap().right_child
    }

    /// Find insertion position for a separator key.
    /// Returns `(found, index)`.
    pub fn find_separator_slot(&self, separator_key: u64) -> (bool, u16) {
        let num_keys = self.num_keys();
        if num_keys == 0 {
            return (false, 0);
        }

        let mut left: u16 = 0;
        let mut right: u16 = num_keys;

        while left < right {
            let mid = left + (right - left) / 2;
            let mid_key = self.entry(mid).unwrap().separator_key;

            match mid_key.cmp(&separator_key) {
                std::cmp::Ordering::Equal => return (true, mid),
                std::cmp::Ordering::Less => left = mid + 1,
                std::cmp::Ordering::Greater => right = mid,
            }
        }

        (false, left)
    }

    // -- Safety checks -------------------------------------------------------

    #[inline]
    pub fn is_safe_for_insert(&self) -> bool {
        self.num_keys() < SAFE_INSERT_THRESHOLD
    }

    #[inline]
    pub fn needs_split(&self) -> bool {
        self.num_keys() >= MAX_KEYS
    }

    #[inline]
    pub fn can_merge(&self) -> bool {
        self.num_keys() < MERGE_THRESHOLD
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.num_keys() == 0
    }

    #[inline]
    pub fn capacity(&self) -> u16 {
        MAX_KEYS
    }

    #[inline]
    pub fn num_children(&self) -> u16 {
        self.num_keys() + 1
    }

    pub fn into_inner(self) -> T {
        self.data
    }
}

// ============================================================================
// MUTABLE METHODS
// ============================================================================

impl<T> DirectoryInnerPage<T>
where
    T: AsRef<[u8]> + AsMut<[u8]>,
{
    /// Initialize a new directory inner page.
    pub fn init(mut data: T, page_id: LPageId, leftmost_child: LPageId) -> Self {
        let buffer = data.as_mut();
        buffer.fill(0);

        // Write UberPageHeader via zerocopy (copy-based)
        let mut uber = UberPageHeader::read_from_prefix(buffer)
            .expect("UberPageHeader must fit")
            .0;
        uber.page_lsn = 0;
        uber.page_id = page_id;
        uber.page_type_id = PageType::DirectoryInner as u8;
        uber.write_to_prefix(buffer)
            .expect("UberPageHeader must fit in page buffer");

        // Write InnerHeader via zerocopy (copy-based)
        let inner = DirectoryInnerHeader {
            num_keys: 0,
            flags: 0,
            leftmost_child,
        };
        inner
            .write_to_prefix(&mut data.as_mut()[UBER_HEADER_SIZE..])
            .expect("DirectoryInnerHeader must fit");

        Self { data }
    }

    #[inline]
    pub fn as_buffer_mut(&mut self) -> &mut [u8] {
        self.data.as_mut()
    }
    /// Read-modify-write the inner header.
    #[inline]
    fn update_inner_header(&mut self, f: impl FnOnce(&mut DirectoryInnerHeader)) {
        let mut header = DirectoryInnerHeader::read_from_prefix(
            &self.data.as_ref()[UBER_HEADER_SIZE..],
        )
        .expect("DirectoryInnerHeader must fit")
        .0;

        f(&mut header);

        header
            .write_to_prefix(&mut self.data.as_mut()[UBER_HEADER_SIZE..])
            .expect("DirectoryInnerHeader must fit");
    }

    #[inline]
    pub fn set_page_lsn(&mut self, lsn: u64) {
        let mut uber = UberPageHeader::read_from_prefix(self.data.as_ref())
            .expect("UberPageHeader must fit")
            .0;
        uber.page_lsn = lsn;
        uber.write_to_prefix(self.data.as_mut())
            .expect("UberPageHeader must fit");
    }

    #[inline]
    fn set_num_keys(&mut self, count: u16) {
        self.update_inner_header(|h| h.num_keys = count);
    }

    #[inline]
    pub fn set_flags(&mut self, flags: u16) {
        self.update_inner_header(|h| h.flags = flags);
    }

    #[inline]
    pub fn set_leftmost_child(&mut self, page_id: LPageId) {
        self.update_inner_header(|h| h.leftmost_child = page_id);
    }

    // -- Entry modification via zerocopy -------------------------------------

    fn set_entry(&mut self, index: u16, entry: DirectoryInnerEntry) {
        assert!(
            index < MAX_KEYS,
            "Entry index {} exceeds capacity {}",
            index,
            MAX_KEYS
        );

        let offset = ENTRIES_OFFSET + (index as usize * ENTRY_SIZE);
        entry
            .write_to_prefix(&mut self.data.as_mut()[offset..])
            .expect("Entry must fit at calculated offset");
    }

    /// Insert a new separator key and child pointer (sorted).
    pub fn insert_separator(
        &mut self,
        separator_key: u64,
        right_child: LPageId,
    ) -> Result<(), OverlayError> {
        let num_keys = self.num_keys();

        if num_keys >= MAX_KEYS {
            return Err(OverlayError::PageFull {
                capacity: MAX_KEYS as usize,
                attempted: num_keys as usize + 1,
            });
        }

        let (found, insert_pos) = self.find_separator_slot(separator_key);
        if found {
            return Err(OverlayError::DuplicateKey { key: separator_key });
        }

        // Shift entries right
        if insert_pos < num_keys {
            let src_start = ENTRIES_OFFSET + (insert_pos as usize * ENTRY_SIZE);
            let dst_start = ENTRIES_OFFSET + ((insert_pos + 1) as usize * ENTRY_SIZE);
            let count = (num_keys - insert_pos) as usize * ENTRY_SIZE;
            self.data
                .as_mut()
                .copy_within(src_start..(src_start + count), dst_start);
        }

        self.set_entry(insert_pos, DirectoryInnerEntry::new(separator_key, right_child));
        self.set_num_keys(num_keys + 1);

        Ok(())
    }

    /// Delete a separator at the given index.
    pub fn delete_separator(&mut self, index: u16) -> Result<(), OverlayError> {
        let num_keys = self.num_keys();
        if index >= num_keys {
            return Err(OverlayError::IndexOutOfBounds {
                index,
                max: num_keys,
            });
        }

        if index < num_keys - 1 {
            let src_start = ENTRIES_OFFSET + ((index + 1) as usize * ENTRY_SIZE);
            let dst_start = ENTRIES_OFFSET + (index as usize * ENTRY_SIZE);
            let count = (num_keys - 1 - index) as usize * ENTRY_SIZE;
            self.data
                .as_mut()
                .copy_within(src_start..(src_start + count), dst_start);
        }

        self.set_num_keys(num_keys - 1);
        Ok(())
    }

    /// Update the right_child pointer for a separator at the given index.
    pub fn update_child(
        &mut self,
        index: u16,
        new_child: LPageId,
    ) -> Result<(), OverlayError> {
        let num_keys = self.num_keys();
        if index >= num_keys {
            return Err(OverlayError::IndexOutOfBounds {
                index,
                max: num_keys,
            });
        }

        let entry = self.entry(index)?;
        self.set_entry(
            index,
            DirectoryInnerEntry::new(entry.separator_key, new_child),
        );
        Ok(())
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capacity_constants() {
        assert_eq!(MAX_KEYS, 254);
        assert_eq!(SAFE_INSERT_THRESHOLD, 253);
        assert_eq!(MERGE_THRESHOLD, 127);
    }

    #[test]
    fn test_entry_structure() {
        let entry = DirectoryInnerEntry::new(100, 5);
        assert_eq!(entry.separator_key, 100);
        assert_eq!(entry.right_child, 5);
    }

    #[test]
    fn test_init() {
        let buffer = [0u8; PAGE_BUF_SIZE];
        let page = DirectoryInnerPage::init(buffer, 42, 99);

        assert_eq!(page.page_id(), 42);
        assert_eq!(page.page_type_byte(), PageType::DirectoryInner as u8);
        assert_eq!(page.num_keys(), 0);
        assert_eq!(page.leftmost_child(), 99);
        assert!(page.is_empty());
        assert!(!page.needs_split());
        assert!(page.is_safe_for_insert());
        assert_eq!(page.num_children(), 1);
    }

    #[test]
    fn test_routing_empty_node() {
        let buffer = [0u8; PAGE_BUF_SIZE];
        let page = DirectoryInnerPage::init(buffer, 1, 99);

        assert_eq!(page.find_child(0), 99);
        assert_eq!(page.find_child(100), 99);
        assert_eq!(page.find_child(u64::MAX), 99);
    }

    #[test]
    fn test_routing_single_separator() {
        let buffer = [0u8; PAGE_BUF_SIZE];
        let mut page = DirectoryInnerPage::init(buffer, 1, 10);

        page.insert_separator(100, 20).unwrap();

        assert_eq!(page.find_child(0), 10);
        assert_eq!(page.find_child(50), 10);
        assert_eq!(page.find_child(99), 10);
        assert_eq!(page.find_child(100), 20);
        assert_eq!(page.find_child(150), 20);
        assert_eq!(page.find_child(u64::MAX), 20);
    }

    #[test]
    fn test_routing_multiple_separators() {
        let buffer = [0u8; PAGE_BUF_SIZE];
        let mut page = DirectoryInnerPage::init(buffer, 1, 10);

        page.insert_separator(100, 20).unwrap();
        page.insert_separator(200, 30).unwrap();
        page.insert_separator(300, 40).unwrap();

        assert_eq!(page.num_keys(), 3);
        assert_eq!(page.num_children(), 4);

        assert_eq!(page.find_child(50), 10);
        assert_eq!(page.find_child(100), 20);
        assert_eq!(page.find_child(150), 20);
        assert_eq!(page.find_child(200), 30);
        assert_eq!(page.find_child(250), 30);
        assert_eq!(page.find_child(300), 40);
        assert_eq!(page.find_child(500), 40);
    }

    #[test]
    fn test_insertion_order() {
        let buffer = [0u8; PAGE_BUF_SIZE];
        let mut page = DirectoryInnerPage::init(buffer, 1, 0);

        page.insert_separator(300, 3).unwrap();
        page.insert_separator(100, 1).unwrap();
        page.insert_separator(200, 2).unwrap();

        assert_eq!(page.entry(0).unwrap().separator_key, 100);
        assert_eq!(page.entry(0).unwrap().right_child, 1);
        assert_eq!(page.entry(1).unwrap().separator_key, 200);
        assert_eq!(page.entry(1).unwrap().right_child, 2);
        assert_eq!(page.entry(2).unwrap().separator_key, 300);
        assert_eq!(page.entry(2).unwrap().right_child, 3);
    }

    #[test]
    fn test_duplicate_separator() {
        let buffer = [0u8; PAGE_BUF_SIZE];
        let mut page = DirectoryInnerPage::init(buffer, 1, 0);
        page.insert_separator(100, 1).unwrap();

        let result = page.insert_separator(100, 2);
        assert!(matches!(result, Err(OverlayError::DuplicateKey { key: 100 })));
    }

    #[test]
    fn test_capacity_enforcement() {
        let buffer = [0u8; PAGE_BUF_SIZE];
        let mut page = DirectoryInnerPage::init(buffer, 1, 0);

        for i in 0..MAX_KEYS {
            page.insert_separator(i as u64, 0).unwrap();
        }

        assert!(page.needs_split());
        assert!(!page.is_safe_for_insert());

        let result = page.insert_separator(999, 0);
        assert!(matches!(result, Err(OverlayError::PageFull { .. })));
    }

    #[test]
    fn test_delete_separator() {
        let buffer = [0u8; PAGE_BUF_SIZE];
        let mut page = DirectoryInnerPage::init(buffer, 1, 0);

        page.insert_separator(100, 1).unwrap();
        page.insert_separator(200, 2).unwrap();
        page.insert_separator(300, 3).unwrap();

        page.delete_separator(1).unwrap();

        assert_eq!(page.num_keys(), 2);
        assert_eq!(page.entry(0).unwrap().separator_key, 100);
        assert_eq!(page.entry(1).unwrap().separator_key, 300);
    }

    #[test]
    fn test_from_buffer_validation() {
        let buffer = [0u8; PAGE_BUF_SIZE];
        let page = DirectoryInnerPage::init(buffer, 1, 0);

        let valid_buffer: [u8; PAGE_BUF_SIZE] = page.as_buffer().try_into().unwrap();
        let result = DirectoryInnerPage::from_buffer(valid_buffer);
        assert!(result.is_ok());

        let mut bad_buffer = [0u8; PAGE_BUF_SIZE];
        bad_buffer[12] = PageType::DirectoryLeaf as u8;
        let result = DirectoryInnerPage::from_buffer(bad_buffer);
        assert!(matches!(result, Err(OverlayError::TypeMismatch { .. })));
    }

    #[test]
    fn test_find_separator_slot() {
        let buffer = [0u8; PAGE_BUF_SIZE];
        let mut page = DirectoryInnerPage::init(buffer, 1, 0);

        page.insert_separator(100, 1).unwrap();
        page.insert_separator(200, 2).unwrap();
        page.insert_separator(300, 3).unwrap();

        assert_eq!(page.find_separator_slot(100), (true, 0));
        assert_eq!(page.find_separator_slot(200), (true, 1));
        assert_eq!(page.find_separator_slot(300), (true, 2));

        assert_eq!(page.find_separator_slot(50), (false, 0));
        assert_eq!(page.find_separator_slot(150), (false, 1));
        assert_eq!(page.find_separator_slot(250), (false, 2));
        assert_eq!(page.find_separator_slot(400), (false, 3));
    }

    #[test]
    fn test_safety_threshold() {
        let buffer = [0u8; PAGE_BUF_SIZE];
        let mut page = DirectoryInnerPage::init(buffer, 1, 0);

        for i in 0..(SAFE_INSERT_THRESHOLD - 1) {
            page.insert_separator(i as u64, 0).unwrap();
        }
        assert!(page.is_safe_for_insert());

        page.insert_separator(SAFE_INSERT_THRESHOLD as u64, 0)
            .unwrap();
        assert!(!page.is_safe_for_insert());
    }
}
