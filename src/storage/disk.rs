use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::{io, sync::Arc};

use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::RwLock;

use crate::{
    common::{
        aliases::{self, FileId, LPageId, PPageId},
        constants::PAGE_BUF_SIZE,
    },
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

    fn new_page(&self) -> impl Future<Output = Result<aliases::LPageId>> + '_ + Send;
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
    /// Cache of open file handles
    file_handles: RwLock<HashMap<FileId, Arc<RwLock<File>>>>,
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
    pub async fn register_file(&self, file_id: FileId, file_type: FileType) {
        let mut types = self.file_types.write().await;
        types.insert(file_id, file_type);
    }

    /// Get the path for a file.
    fn file_path(&self, file_id: FileId, file_type: FileType) -> PathBuf {
        self.base_path
            .join(format!("{}.{}", file_id, file_type.extension()))
    }

    /// Get or open a file handle.
    async fn get_file(&self, file_id: FileId) -> Result<Arc<RwLock<File>>> {
        // Check cache first
        {
            let handles = self.file_handles.read().await;
            if let Some(handle) = handles.get(&file_id) {
                return Ok(handle.clone());
            }
        }

        // Get file type
        let file_type = {
            let types = self.file_types.read().await;
            types.get(&file_id).copied().unwrap_or(FileType::Heap)
        };

        // Open file
        let path = self.file_path(file_id, file_type);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)
            .await?;

        let handle = Arc::new(RwLock::new(file));

        // Cache it
        {
            let mut handles = self.file_handles.write().await;
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

impl<D: directory::PageDirectory, A: allocator::PageAllocator> DiskManager for DiskManagerImpl<D, A> {
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
            let handle = self.get_file(loc.file).await?;
            let mut file = handle.write().await;

            file.seek(std::io::SeekFrom::Start(loc.offset)).await?;
            file.read_exact(target).await?;

            Ok(())
        }
    }

    fn write_page_at_loc<'a>(
        &'a self,
        loc: PPageId,
        target: &'a aliases::PageBuffer,
    ) -> impl Future<Output = Result<()>> + 'a + Send {
        async move {
            let handle = self.get_file(loc.file).await?;
            let mut file = handle.write().await;

            file.seek(std::io::SeekFrom::Start(loc.offset)).await?;
            file.write_all(target).await?;
            // Note: sync is not called here for performance; use flush_all_dirty for durability

            Ok(())
        }
    }

    fn new_page(&self) -> impl Future<Output = Result<LPageId>> + '_ + Send {
        async move {
            // Allocate a new logical page ID
            let lpage_id = self.alloc_lpage_id();

            // For now, we need to know which file this page belongs to.
            // This is typically determined by the caller (accessor layer).
            // The new_page in DiskManager is a bit awkward because it doesn't
            // know which file to allocate in.
            //
            // For a proper implementation, new_page should take a file_id parameter
            // or this should be handled at a higher level (accessor).
            //
            // For now, return the lpage_id. The physical allocation happens
            // when the page is first written via the accessor layer which
            // knows the file context.

            Ok(lpage_id)
        }
    }
}

/// Create a new page in a specific file and register it in the directory.
///
/// This is a helper function that combines allocation and directory registration.
pub async fn allocate_page_in_file<D: directory::PageDirectory, A: allocator::PageAllocator>(
    disk_manager: &DiskManagerImpl<D, A>,
    page_directory: &D,
    allocator: &A,
    file_id: FileId,
) -> Result<(LPageId, PPageId)> {
    // Allocate logical page ID
    let lpage_id = disk_manager.alloc_lpage_id();

    // Allocate physical location in file
    let ppage_id = allocator.allocate(file_id).await?;

    // Register in page directory
    page_directory.add_page(lpage_id, ppage_id).await?;

    Ok((lpage_id, ppage_id))
}
