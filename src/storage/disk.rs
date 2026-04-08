use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, RwLock};

use crate::{
    common::aliases::{self, FileId, LPageId, PPageId},
    storage::{allocator, directory},
};

#[derive(Clone, Debug)]
pub enum DiskError {
    IOError(Arc<io::Error>),
    DirectoryError(directory::Error),
    AllocatorError(allocator::Error),
}

impl From<io::Error> for DiskError {
    fn from(e: io::Error) -> Self {
        DiskError::IOError(Arc::new(e))
    }
}

impl From<directory::Error> for DiskError {
    fn from(e: directory::Error) -> Self {
        DiskError::DirectoryError(e)
    }
}

impl From<allocator::Error> for DiskError {
    fn from(e: allocator::Error) -> Self {
        DiskError::AllocatorError(e)
    }
}

pub type Result<V> = std::result::Result<V, DiskError>;

pub trait DiskManager: Send + Sync + 'static {
    fn read_page<'a>(
        &'a self,
        id: aliases::LPageId,
        target: &'a mut aliases::PageBuffer,
    ) -> impl Future<Output = Result<aliases::PPageId>> + 'a + Send;

    fn write_page<'a>(
        &'a self,
        id: aliases::LPageId,
        target: &'a aliases::PageBuffer,
    ) -> impl Future<Output = Result<()>> + 'a + Send;

    fn read_page_at_loc<'a>(
        &'a self,
        loc: aliases::PPageId,
        target: &'a mut aliases::PageBuffer,
    ) -> impl Future<Output = Result<()>> + 'a + Send;

    fn write_page_at_loc<'a>(
        &'a self,
        loc: aliases::PPageId,
        target: &'a aliases::PageBuffer,
    ) -> impl Future<Output = Result<()>> + 'a + Send;

    fn new_page(
        &self,
        file_id: aliases::FileId,
    ) -> impl Future<Output = Result<(aliases::LPageId, aliases::PPageId)>> + '_ + Send;

    /// Flush storage metadata (e.g. page directory) to disk.
    fn flush_metadata(&self) -> impl Future<Output = Result<()>> + '_ + Send;

    /// Pre-initialize a physical page location by allocating an LPageId,
    /// registering the mapping in the page directory, and writing a zero page
    /// to disk so subsequent reads succeed. Used to create file-header pages
    /// at a known physical location (typically offset 0 of a fresh heap file).
    fn init_page_at_loc(
        &self,
        loc: aliases::PPageId,
    ) -> impl Future<Output = Result<aliases::LPageId>> + '_ + Send;
}

// ============================================================================
// FILE TYPE
// ============================================================================

/// Type of database file, determines file extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileType {
    /// Heap file for table data (.heap)
    Heap,
    /// Index file for B-tree indexes (.idx)
    Index,
    /// Directory file for page directory (.dir)
    Directory,
}

impl FileType {
    /// Get the file extension for this type.
    pub fn extension(&self) -> &'static str {
        match self {
            FileType::Heap => "heap",
            FileType::Index => "idx",
            FileType::Directory => "dir",
        }
    }
}

// ============================================================================
// DISK MANAGER IMPLEMENTATION
// ============================================================================

/// A concrete DiskManager implementation that uses a PageDirectory and PageAllocator.
///
/// Files are stored in a base directory with names `{file_id}.{extension}`.
pub struct DiskManagerImpl<D: directory::PageDirectory, A: allocator::PageAllocator> {
    /// Base directory for all database files
    base_path: PathBuf,
    /// Page directory for LPageId -> PPageId translation
    page_directory: Arc<D>,
    /// Page allocator for new page allocation
    allocator: Arc<A>,
    /// Next logical page ID to allocate
    next_lpage_id: AtomicU32,
    /// Cache of open file handles (using std RwLock for sync access with positioned I/O)
    file_handles: RwLock<HashMap<FileId, Arc<File>>>,
    /// Mapping of FileId -> FileType for path construction
    file_types: RwLock<HashMap<FileId, FileType>>,
}

impl<D: directory::PageDirectory, A: allocator::PageAllocator> DiskManagerImpl<D, A> {
    /// Create a new DiskManager.
    ///
    /// # Arguments
    /// * `base_path` - Base directory for database files
    /// * `page_directory` - Page directory for address translation
    /// * `allocator` - Page allocator for new pages
    /// * `next_lpage_id` - Starting logical page ID (should be loaded from metadata)
    pub fn new(
        base_path: PathBuf,
        page_directory: Arc<D>,
        allocator: Arc<A>,
        next_lpage_id: LPageId,
    ) -> Self {
        Self {
            base_path,
            page_directory,
            allocator,
            next_lpage_id: AtomicU32::new(next_lpage_id),
            file_handles: RwLock::new(HashMap::new()),
            file_types: RwLock::new(HashMap::new()),
        }
    }

    /// Register a file with its type.
    pub fn register_file(&self, file_id: FileId, file_type: FileType) {
        let mut types = self.file_types.write().unwrap();
        types.insert(file_id, file_type);
        
        let path = self.file_path(file_id, file_type);
        let pages = if path.exists() {
            let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let count = (len / crate::common::constants::PAGE_BUF_SIZE as u64) as u32;
            if count == 0 { 1 } else { count }
        } else {
            1
        };
        self.allocator.register_file(file_id, pages);
    }

    /// Get the page directory.
    pub fn page_directory(&self) -> &Arc<D> {
        &self.page_directory
    }

    /// Get the page allocator.
    pub fn allocator(&self) -> &Arc<A> {
        &self.allocator
    }

    /// Get the path for a file.
    fn file_path(&self, file_id: FileId, file_type: FileType) -> PathBuf {
        self.base_path
            .join(format!("{}.{}", file_id, file_type.extension()))
    }

    /// Get or open a file handle.
    fn get_file(&self, file_id: FileId) -> Result<Arc<File>> {
        // Check cache first
        {
            let handles = self.file_handles.read().unwrap();
            if let Some(handle) = handles.get(&file_id) {
                return Ok(handle.clone());
            }
        }

        // Get file type
        let file_type = {
            let types = self.file_types.read().unwrap();
            types.get(&file_id).copied().unwrap_or(FileType::Heap)
        };

        // Open file
        let path = self.file_path(file_id, file_type);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)?;

        let handle = Arc::new(file);

        // Cache it
        {
            let mut handles = self.file_handles.write().unwrap();
            handles.insert(file_id, handle.clone());
        }

        Ok(handle)
    }

    /// Get the current next logical page ID.
    pub fn next_lpage_id(&self) -> LPageId {
        self.next_lpage_id.load(Ordering::SeqCst)
    }

    /// Allocate a new logical page ID.
    fn alloc_lpage_id(&self) -> LPageId {
        self.next_lpage_id.fetch_add(1, Ordering::SeqCst)
    }
}

impl<D: directory::PageDirectory, A: allocator::PageAllocator> DiskManager
    for DiskManagerImpl<D, A>
{
    fn read_page<'a>(
        &'a self,
        id: LPageId,
        target: &'a mut aliases::PageBuffer,
    ) -> impl Future<Output = Result<PPageId>> + 'a + Send {
        async move {
            // Look up physical location
            let ppage_id = self.page_directory.lookup(id).await?;

            // Read from disk
            self.read_page_at_loc(ppage_id, target).await?;

            Ok(ppage_id)
        }
    }

    fn write_page<'a>(
        &'a self,
        id: LPageId,
        target: &'a aliases::PageBuffer,
    ) -> impl Future<Output = Result<()>> + 'a + Send {
        async move {
            // Look up physical location
            let ppage_id = self.page_directory.lookup(id).await?;

            // Write to disk
            self.write_page_at_loc(ppage_id, target).await?;

            Ok(())
        }
    }

    fn read_page_at_loc<'a>(
        &'a self,
        loc: PPageId,
        target: &'a mut aliases::PageBuffer,
    ) -> impl Future<Output = Result<()>> + 'a + Send {
        async move {
            let file = self.get_file(loc.file)?;

            tokio::task::block_in_place(|| file.read_exact_at(target, loc.offset))?;

            Ok(())
        }
    }

    fn write_page_at_loc<'a>(
        &'a self,
        loc: PPageId,
        target: &'a aliases::PageBuffer,
    ) -> impl Future<Output = Result<()>> + 'a + Send {
        async move {
            let file = self.get_file(loc.file)?;

            tokio::task::block_in_place(|| file.write_all_at(target, loc.offset))?;

            Ok(())
        }
    }

    fn new_page(
        &self,
        file_id: FileId,
    ) -> impl Future<Output = Result<(LPageId, PPageId)>> + '_ + Send {
        async move {
            // Allocate a new logical page ID
            let lpage_id = self.alloc_lpage_id();

            // Allocate physical location in the file
            let ppage_id = self.allocator.allocate(file_id).await?;

            // Register in the page directory
            self.page_directory.add_page(lpage_id, ppage_id).await?;

            Ok((lpage_id, ppage_id))
        }
    }

    fn flush_metadata(&self) -> impl Future<Output = Result<()>> + '_ + Send {
        async move {
            let next_lpage = self.next_lpage_id.load(Ordering::SeqCst);
            self.page_directory.update_header_state(next_lpage).await?;
            self.page_directory.flush_all_dirty().await?;
            Ok(())
        }
    }

    fn init_page_at_loc(
        &self,
        loc: PPageId,
    ) -> impl Future<Output = Result<LPageId>> + '_ + Send {
        async move {
            // Allocate a fresh logical page id for this physical location.
            let lpage_id = self.alloc_lpage_id();

            // Register the mapping so future logical fetches resolve.
            self.page_directory.add_page(lpage_id, loc).await?;

            // Physically extend the file with a zero page so subsequent
            // reads do not EOF. Direct write to avoid buffer-pool recursion.
            let zero = [0u8; crate::common::constants::PAGE_BUF_SIZE];
            self.write_page_at_loc(loc, &zero).await?;

            Ok(lpage_id)
        }
    }
}
