use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use futures::io;

use crate::common::{
    aliases::{FileId, PPageId},
    constants::PAGE_BUF_SIZE,
};

#[derive(Clone, Debug)]
pub enum Error {
    IOError(Arc<io::Error>),
}

pub type Result<V> = std::result::Result<V, Error>;

/// This component handles tracking allocation and deallocation of pages in files
/// It may also expand/truncate files when appropriate
pub trait PageAllocator: Send + Sync + 'static {
    fn allocate(&self, file_id: FileId) -> impl Future<Output = Result<PPageId>> + Send;
    fn deallocate(&self, physical_id: PPageId) -> impl Future<Output = Result<()>> + Send;
    fn register_file(&self, file_id: FileId, current_page_count: u32);
}

// ============================================================================
// SIMPLE END-OF-FILE ALLOCATOR
// ============================================================================

/// A simple page allocator that always allocates at the end of file.
///
/// This allocator maintains a counter for each file tracking the next available
/// page number. Allocation increments the counter and returns the new page.
/// Deallocation is a no-op (pages are not reclaimed).
///
/// This is suitable for initial development but should be replaced with a
/// more sophisticated allocator that tracks free pages for production use.
pub struct SimpleAllocator {
    /// Maps FileId -> next page number (0-indexed, so first data page is 1 since page 0 is header)
    next_page: RwLock<HashMap<FileId, u32>>,
}

impl SimpleAllocator {
    /// Create a new allocator with no files registered.
    pub fn new() -> Self {
        Self {
            next_page: RwLock::new(HashMap::new()),
        }
    }

    /// Get the current page count for a file.
    pub fn page_count(&self, file_id: FileId) -> Option<u32> {
        let map = self.next_page.read().expect("allocator lock poisoned");
        map.get(&file_id).copied()
    }
}

impl Default for SimpleAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl PageAllocator for SimpleAllocator {
    fn allocate(&self, file_id: FileId) -> impl Future<Output = Result<PPageId>> + Send {
        async move {
            let page_num = {
                let mut map = self.next_page.write().expect("allocator lock poisoned");
                let next = map.entry(file_id).or_insert(1); // Start at 1 (page 0 is header)
                let allocated = *next;
                *next += 1;
                allocated
            };

            Ok(PPageId {
                file: file_id,
                offset: page_num as u64 * PAGE_BUF_SIZE as u64,
            })
        }
    }

    fn deallocate(&self, _physical_id: PPageId) -> impl Future<Output = Result<()>> + Send {
        // Simple allocator does not reclaim pages - this is a no-op
        async { Ok(()) }
    }

    /// Register a file with its current page count.
    ///
    /// This should be called when opening existing files to set the
    /// starting point for new allocations.
    fn register_file(&self, file_id: FileId, current_page_count: u32) {
        let mut map = self.next_page.write().expect("allocator lock poisoned");
        map.insert(file_id, current_page_count);
    }
}
