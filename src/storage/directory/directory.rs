use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::RwLock;

use crate::common::{
    aliases::{self, DirPageId, LPageId, PPageId},
    constants::PAGE_BUF_SIZE,
};
use crate::page::{
    PageType, UberPageHeader,
    overlays::{
        directory::{
            DirectoryInnerEntry, DirectoryInnerPage, DirectoryLeafPage, INNER_MAX_KEYS,
            LEAF_MAX_ENTRIES,
        },
        file_header::DirectoryFileHeaderPage,
    },
};

use zerocopy::FromBytes;

#[derive(Clone, Debug)]
pub enum Error {
    IOError(Arc<io::Error>),
    PageNotFound(LPageId),
    PageCorruption(String),
    DuplicatePage(LPageId),
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::IOError(Arc::new(e))
    }
}

pub type Result<V> = std::result::Result<V, Error>;

pub trait PageDirectory: Send + Sync + 'static {
    fn lookup(
        &self,
        page_id: aliases::LPageId,
    ) -> impl Future<Output = Result<aliases::PPageId>> + '_ + Send;
    fn add_page(
        &self,
        page_id: aliases::LPageId,
        physical_id: aliases::PPageId,
    ) -> impl Future<Output = Result<()>> + '_ + Send;
    fn update_page(
        &self,
        page_id: aliases::LPageId,
        physical_id: aliases::PPageId,
    ) -> impl Future<Output = Result<()>> + '_ + Send;
    fn delete_page(
        &self,
        page_id: aliases::LPageId,
    ) -> impl Future<Output = Result<()>> + '_ + Send;

    /// Force flush changes to disk
    fn flush_all_dirty(&self) -> impl Future<Output = Result<()>> + Send;
    fn get_next_lpage_id(&self) -> impl Future<Output = Result<aliases::LPageId>> + '_ + Send;
    fn update_next_lpage_id(&self, lpage_id: LPageId) -> impl Future<Output = Result<()>> + '_ + Send;
}

// ============================================================================
// B-TREE PAGE DIRECTORY IMPLEMENTATION
// ============================================================================

/// A B-tree based page directory stored in a .dir file.
///
/// Maps logical page IDs (LPageId) to physical page IDs (PPageId).
/// The B-tree uses DirectoryInnerPage and DirectoryLeafPage overlays.
///
/// Pages within the directory file are identified by DirPageId, where
/// offset = DirPageId * PAGE_SIZE.
pub struct BTreePageDirectory {
    /// Path to the .dir file
    file_path: PathBuf,
    /// Cached directory pages (DirPageId -> page buffer)
    cache: RwLock<HashMap<DirPageId, Box<[u8; PAGE_BUF_SIZE]>>>,
    /// Set of dirty page IDs that need to be flushed
    dirty: RwLock<std::collections::HashSet<DirPageId>>,
    /// Next available DirPageId for allocation
    next_dir_page: RwLock<DirPageId>,
    /// Root page of the B-tree (cached from header)
    root_page: RwLock<DirPageId>,
}

impl BTreePageDirectory {
    /// Open or create a page directory at the given path.
    pub async fn open(file_path: PathBuf) -> Result<Self> {
        let exists = file_path.exists();

        let dir = Self {
            file_path: file_path.clone(),
            cache: RwLock::new(HashMap::new()),
            dirty: RwLock::new(std::collections::HashSet::new()),
            next_dir_page: RwLock::new(1), // Page 0 is header
            root_page: RwLock::new(0),     // 0 = no root yet
        };

        if exists {
            // Load existing directory
            dir.load_header().await?;
        } else {
            // Initialize new directory file
            dir.init_new().await?;
        }

        Ok(dir)
    }

    /// Initialize a new directory file with header page.
    async fn init_new(&self) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&self.file_path)
            .await?;

        // Create and write header page
        let mut buffer = [0u8; PAGE_BUF_SIZE];
        let header = DirectoryFileHeaderPage::init(&mut buffer, 0);
        // Header starts with next_dir_page = 1 (header is page 0)
        // root_page = 0 means empty tree

        file.write_all(header.as_buffer()).await?;
        file.sync_all().await?;

        Ok(())
    }

    /// Load header from existing directory file.
    async fn load_header(&self) -> Result<()> {
        let mut file = File::open(&self.file_path).await?;
        let mut buffer = [0u8; PAGE_BUF_SIZE];
        file.read_exact(&mut buffer).await?;

        let header = DirectoryFileHeaderPage::from_buffer(&buffer)
            .map_err(|e| Error::PageCorruption(format!("invalid directory header: {:?}", e)))?;

        let data = header
            .data()
            .map_err(|e| Error::PageCorruption(format!("cannot read header data: {:?}", e)))?;

        *self.root_page.write().await = data.dir_root_page;
        *self.next_dir_page.write().await = if data.next_dir_page == 0 { 1 } else { data.next_dir_page };

        Ok(())
    }

    /// Read a directory page from disk or cache.
    async fn read_page(&self, dir_page_id: DirPageId) -> Result<Box<[u8; PAGE_BUF_SIZE]>> {
        // Check cache first
        {
            let cache = self.cache.read().await;
            if let Some(page) = cache.get(&dir_page_id) {
                return Ok(page.clone());
            }
        }

        // Read from disk
        let mut file = File::open(&self.file_path).await?;
        let offset = dir_page_id as u64 * PAGE_BUF_SIZE as u64;
        file.seek(std::io::SeekFrom::Start(offset)).await?;

        let mut buffer = Box::new([0u8; PAGE_BUF_SIZE]);
        file.read_exact(buffer.as_mut()).await?;

        // Cache the page
        {
            let mut cache = self.cache.write().await;
            cache.insert(dir_page_id, buffer.clone());
        }

        Ok(buffer)
    }

    /// Write a page to cache and mark as dirty.
    async fn write_page(&self, dir_page_id: DirPageId, buffer: Box<[u8; PAGE_BUF_SIZE]>) {
        let mut cache = self.cache.write().await;
        cache.insert(dir_page_id, buffer);
        drop(cache);

        let mut dirty = self.dirty.write().await;
        dirty.insert(dir_page_id);
    }

    /// Allocate a new directory page.
    async fn alloc_dir_page(&self) -> Result<DirPageId> {
        let allocated = {
            let mut next = self.next_dir_page.write().await;
            let val = *next;
            *next += 1;
            val
        };
        
        let mut buffer = *self.read_page(0).await?;
        {
            let mut header = DirectoryFileHeaderPage::new(&mut buffer);
            let data = header.data_mut().map_err(|e| Error::PageCorruption(format!("cannot write header: {:?}", e)))?;
            data.next_dir_page = allocated + 1;
        }
        self.write_page(0, Box::new(buffer)).await;
        Ok(allocated)
    }

    /// Update the root page in header.
    async fn set_root_page(&self, new_root: DirPageId) -> Result<()> {
        *self.root_page.write().await = new_root;

        // Update header page
        let mut buffer = *self.read_page(0).await?;
        {
            let mut header = DirectoryFileHeaderPage::new(&mut buffer);
            let data = header
                .data_mut()
                .map_err(|e| Error::PageCorruption(format!("cannot write header: {:?}", e)))?;
            data.dir_root_page = new_root;
        }
        self.write_page(0, Box::new(buffer)).await;

        Ok(())
    }

    /// Find the leaf page containing the given key.
    async fn find_leaf(&self, key: u64) -> Result<DirPageId> {
        let root = *self.root_page.read().await;
        if root == 0 {
            return Err(Error::PageNotFound(key as LPageId));
        }

        let mut current = root;
        loop {
            let buffer = self.read_page(current).await?;
            let uber = UberPageHeader::read_from_prefix(buffer.as_ref())
                .map_err(|_| Error::PageCorruption("cannot read UberPageHeader".into()))?
                .0;
            let page_type = PageType::try_from(uber.page_type_id).map_err(|_| {
                Error::PageCorruption(format!("invalid page type {}", uber.page_type_id))
            })?;

            match page_type {
                PageType::DirectoryInner => {
                    let page = DirectoryInnerPage::new(buffer.as_ref());
                    current = page.find_child(key);
                }
                PageType::DirectoryLeaf => {
                    return Ok(current);
                }
                _ => {
                    return Err(Error::PageCorruption(format!(
                        "unexpected page type {:?} in directory B-tree",
                        page_type
                    )));
                }
            }
        }
    }

    /// Find leaf with path for insert/delete operations.
    async fn find_leaf_with_path(&self, key: u64) -> Result<(DirPageId, Vec<DirPageId>)> {
        let root = *self.root_page.read().await;
        if root == 0 {
            return Ok((0, Vec::new()));
        }

        let mut current = root;
        let mut path = Vec::new();

        loop {
            let buffer = self.read_page(current).await?;
            let uber = UberPageHeader::read_from_prefix(buffer.as_ref())
                .map_err(|_| Error::PageCorruption("cannot read UberPageHeader".into()))?
                .0;
            let page_type = PageType::try_from(uber.page_type_id).map_err(|_| {
                Error::PageCorruption(format!("invalid page type {}", uber.page_type_id))
            })?;

            match page_type {
                PageType::DirectoryInner => {
                    path.push(current);
                    let page = DirectoryInnerPage::new(buffer.as_ref());
                    current = page.find_child(key);
                }
                PageType::DirectoryLeaf => {
                    return Ok((current, path));
                }
                _ => {
                    return Err(Error::PageCorruption(format!(
                        "unexpected page type {:?} in directory B-tree",
                        page_type
                    )));
                }
            }
        }
    }

    /// Split a leaf page.
    async fn split_leaf(
        &self,
        leaf_id: DirPageId,
        leaf_buf: &mut [u8; PAGE_BUF_SIZE],
        extra_key: LPageId,
        extra_ppage: PPageId,
    ) -> Result<(DirPageId, u64)> {
        // Collect all entries including the new one
        let mut entries = Vec::new();
        {
            let page = DirectoryLeafPage::new(leaf_buf.as_ref());
            let num = page.num_entries();
            for i in 0..num {
                let entry = page.entry(i).map_err(|e| {
                    Error::PageCorruption(format!("cannot read leaf entry: {:?}", e))
                })?;
                entries.push((entry.logical_page_id(), entry.to_ppage_id()));
            }
        }

        // Insert new entry in sorted position
        let search_key = extra_key as u64;
        let pos = entries
            .binary_search_by_key(&search_key, |(k, _)| *k as u64)
            .err()
            .ok_or_else(|| Error::DuplicatePage(extra_key))?;
        entries.insert(pos, (extra_key, extra_ppage));

        // Split at midpoint
        let mid = entries.len() / 2;
        let separator = entries[mid].0 as u64;

        // Allocate new leaf for upper half
        let new_leaf_id = self.alloc_dir_page().await?;
        let mut new_buf = Box::new([0u8; PAGE_BUF_SIZE]);
        {
            let mut new_page = DirectoryLeafPage::init(new_buf.as_mut(), new_leaf_id);
            for (k, p) in &entries[mid..] {
                new_page
                    .insert_entry(*k, *p)
                    .map_err(|e| Error::PageCorruption(format!("split insert failed: {:?}", e)))?;
            }
            new_page.set_next_page(DirectoryLeafPage::new(leaf_buf.as_ref()).next_page());
            new_page.set_prev_page(leaf_id);
        }
        self.write_page(new_leaf_id, new_buf).await;

        // Rebuild old leaf with lower half
        let old_next = DirectoryLeafPage::new(leaf_buf.as_ref()).next_page();
        let old_prev = DirectoryLeafPage::new(leaf_buf.as_ref()).prev_page();
        {
            let mut old_page = DirectoryLeafPage::init(leaf_buf, leaf_id);
            for (k, p) in &entries[..mid] {
                old_page.insert_entry(*k, *p).map_err(|e| {
                    Error::PageCorruption(format!("split old leaf insert failed: {:?}", e))
                })?;
            }
            old_page.set_next_page(new_leaf_id);
            old_page.set_prev_page(old_prev);
        }

        // Update old right sibling's prev pointer
        if old_next != 0 {
            let mut sib_buf = *self.read_page(old_next).await?;
            {
                let mut sib_page = DirectoryLeafPage::new(&mut sib_buf);
                sib_page.set_prev_page(new_leaf_id);
            }
            self.write_page(old_next, Box::new(sib_buf)).await;
        }

        Ok((new_leaf_id, separator))
    }

    /// Split an inner page.
    async fn split_inner(
        &self,
        inner_id: DirPageId,
        inner_buf: &mut [u8; PAGE_BUF_SIZE],
        sep_key: u64,
        right_child: DirPageId,
    ) -> Result<(DirPageId, u64)> {
        // Collect all entries including the new one
        let mut entries = Vec::new();
        let old_leftmost;
        {
            let page = DirectoryInnerPage::new(inner_buf.as_ref());
            old_leftmost = page.leftmost_child();
            let num = page.num_keys();
            for i in 0..num {
                let entry = page.entry(i).map_err(|e| {
                    Error::PageCorruption(format!("cannot read inner entry: {:?}", e))
                })?;
                entries.push(entry);
            }
        }

        // Insert new separator in sorted position
        let pos = entries
            .binary_search_by_key(&sep_key, |e| e.separator_key)
            .err()
            .ok_or_else(|| Error::PageCorruption("duplicate separator in inner split".into()))?;
        entries.insert(pos, DirectoryInnerEntry::new(sep_key, right_child));

        // Split: entries[..mid] stay, entries[mid] pushed up, entries[mid+1..] -> new page
        let mid = entries.len() / 2;
        let pushed_up = entries[mid].separator_key;
        let new_leftmost = entries[mid].right_child;

        // Allocate new inner page for upper half
        let new_inner_id = self.alloc_dir_page().await?;
        let mut new_buf = Box::new([0u8; PAGE_BUF_SIZE]);
        {
            let mut new_page =
                DirectoryInnerPage::init(new_buf.as_mut(), new_inner_id, new_leftmost);
            for entry in &entries[mid + 1..] {
                new_page
                    .insert_separator(entry.separator_key, entry.right_child)
                    .map_err(|e| {
                        Error::PageCorruption(format!("split inner insert failed: {:?}", e))
                    })?;
            }
        }
        self.write_page(new_inner_id, new_buf).await;

        // Rebuild old inner page with lower half
        {
            let mut old_page = DirectoryInnerPage::init(inner_buf, inner_id, old_leftmost);
            for entry in &entries[..mid] {
                old_page
                    .insert_separator(entry.separator_key, entry.right_child)
                    .map_err(|e| {
                        Error::PageCorruption(format!("split old inner insert failed: {:?}", e))
                    })?;
            }
        }

        Ok((new_inner_id, pushed_up))
    }

    /// Insert separator into ancestors, handling splits.
    async fn insert_into_ancestors(
        &self,
        path: &[DirPageId],
        mut sep_key: u64,
        mut new_child: DirPageId,
    ) -> Result<()> {
        for &parent_id in path.iter().rev() {
            let mut parent_buf = *self.read_page(parent_id).await?;

            let is_full = {
                let page = DirectoryInnerPage::new(parent_buf.as_ref());
                page.num_keys() >= INNER_MAX_KEYS
            };

            if !is_full {
                // Room to insert
                {
                    let mut page = DirectoryInnerPage::new(&mut parent_buf);
                    page.insert_separator(sep_key, new_child).map_err(|e| {
                        Error::PageCorruption(format!("ancestor insert failed: {:?}", e))
                    })?;
                }
                self.write_page(parent_id, Box::new(parent_buf)).await;
                return Ok(());
            }

            // Split the inner page
            let (new_inner, pushed_up) = self
                .split_inner(parent_id, &mut parent_buf, sep_key, new_child)
                .await?;
            self.write_page(parent_id, Box::new(parent_buf)).await;

            sep_key = pushed_up;
            new_child = new_inner;
        }

        // All ancestors split - create new root
        let old_root = *self.root_page.read().await;
        let new_root_id = self.alloc_dir_page().await?;
        let mut new_root_buf = Box::new([0u8; PAGE_BUF_SIZE]);
        {
            let mut root_page =
                DirectoryInnerPage::init(new_root_buf.as_mut(), new_root_id, old_root);
            root_page
                .insert_separator(sep_key, new_child)
                .map_err(|e| Error::PageCorruption(format!("new root insert failed: {:?}", e)))?;
        }
        self.write_page(new_root_id, new_root_buf).await;
        self.set_root_page(new_root_id).await?;

        Ok(())
    }
}

impl PageDirectory for BTreePageDirectory {
    fn lookup(&self, page_id: LPageId) -> impl Future<Output = Result<PPageId>> + '_ + Send {
        async move {
            let root = *self.root_page.read().await;
            if root == 0 {
                return Err(Error::PageNotFound(page_id));
            }

            let leaf_id = self.find_leaf(page_id as u64).await?;
            let buffer = self.read_page(leaf_id).await?;
            let page = DirectoryLeafPage::new(buffer.as_ref());

            page.lookup(page_id as u64)
                .ok_or(Error::PageNotFound(page_id))
        }
    }

    fn add_page(
        &self,
        page_id: LPageId,
        physical_id: PPageId,
    ) -> impl Future<Output = Result<()>> + '_ + Send {
        async move {
            let root = *self.root_page.read().await;

            // Handle empty tree
            if root == 0 {
                let new_leaf_id = self.alloc_dir_page().await?;
                let mut buffer = Box::new([0u8; PAGE_BUF_SIZE]);
                {
                    let mut page = DirectoryLeafPage::init(buffer.as_mut(), new_leaf_id);
                    page.insert_entry(page_id, physical_id).map_err(|e| {
                        Error::PageCorruption(format!("leaf insert failed: {:?}", e))
                    })?;
                }
                self.write_page(new_leaf_id, buffer).await;
                self.set_root_page(new_leaf_id).await?;
                return Ok(());
            }

            // Find leaf and path
            let (leaf_id, path) = self.find_leaf_with_path(page_id as u64).await?;

            if leaf_id == 0 {
                // Empty tree case already handled above
                return Err(Error::PageCorruption("unexpected empty tree".into()));
            }

            let mut leaf_buf = *self.read_page(leaf_id).await?;

            let needs_split = {
                let page = DirectoryLeafPage::new(leaf_buf.as_ref());
                page.num_entries() >= LEAF_MAX_ENTRIES
            };

            if needs_split {
                let (new_leaf, sep) = self
                    .split_leaf(leaf_id, &mut leaf_buf, page_id, physical_id)
                    .await?;
                self.write_page(leaf_id, Box::new(leaf_buf)).await;

                if path.is_empty() {
                    // Leaf was root - create new root
                    let new_root_id = self.alloc_dir_page().await?;
                    let mut new_root_buf = Box::new([0u8; PAGE_BUF_SIZE]);
                    {
                        let mut root_page =
                            DirectoryInnerPage::init(new_root_buf.as_mut(), new_root_id, leaf_id);
                        root_page.insert_separator(sep, new_leaf).map_err(|e| {
                            Error::PageCorruption(format!("new root insert failed: {:?}", e))
                        })?;
                    }
                    self.write_page(new_root_id, new_root_buf).await;
                    self.set_root_page(new_root_id).await?;
                } else {
                    self.insert_into_ancestors(&path, sep, new_leaf).await?;
                }
            } else {
                // Simple insert
                {
                    let mut page = DirectoryLeafPage::new(&mut leaf_buf);
                    page.insert_entry(page_id, physical_id).map_err(|e| {
                        Error::PageCorruption(format!("leaf insert failed: {:?}", e))
                    })?;
                }
                self.write_page(leaf_id, Box::new(leaf_buf)).await;
            }

            Ok(())
        }
    }

    fn update_page(
        &self,
        page_id: LPageId,
        physical_id: PPageId,
    ) -> impl Future<Output = Result<()>> + '_ + Send {
        async move {
            let root = *self.root_page.read().await;
            if root == 0 {
                return Err(Error::PageNotFound(page_id));
            }

            let leaf_id = self.find_leaf(page_id as u64).await?;
            let mut buffer = *self.read_page(leaf_id).await?;

            let updated = {
                let mut page = DirectoryLeafPage::new(&mut buffer);
                page.update_entry(page_id, physical_id)
                    .map_err(|e| Error::PageCorruption(format!("update failed: {:?}", e)))?
            };

            if !updated {
                return Err(Error::PageNotFound(page_id));
            }

            self.write_page(leaf_id, Box::new(buffer)).await;
            Ok(())
        }
    }

    fn delete_page(&self, page_id: LPageId) -> impl Future<Output = Result<()>> + '_ + Send {
        async move {
            let root = *self.root_page.read().await;
            if root == 0 {
                return Err(Error::PageNotFound(page_id));
            }

            let leaf_id = self.find_leaf(page_id as u64).await?;
            let mut buffer = *self.read_page(leaf_id).await?;

            let deleted = {
                let mut page = DirectoryLeafPage::new(&mut buffer);
                page.delete_by_key(page_id as u64)
                    .map_err(|e| Error::PageCorruption(format!("delete failed: {:?}", e)))?
            };

            if !deleted {
                return Err(Error::PageNotFound(page_id));
            }

            self.write_page(leaf_id, Box::new(buffer)).await;

            // Note: We don't implement merging for simplicity. Underfull pages
            // remain in the tree but are functionally correct.

            Ok(())
        }
    }

    fn flush_all_dirty(&self) -> impl Future<Output = Result<()>> + Send {
        async move {
            let dirty_pages: Vec<DirPageId> = {
                let dirty = self.dirty.read().await;
                dirty.iter().copied().collect()
            };

            if dirty_pages.is_empty() {
                return Ok(());
            }

            let mut file = OpenOptions::new().write(true).open(&self.file_path).await?;

            let cache = self.cache.read().await;
            for dir_page_id in &dirty_pages {
                if let Some(buffer) = cache.get(dir_page_id) {
                    let offset = *dir_page_id as u64 * PAGE_BUF_SIZE as u64;
                    file.seek(std::io::SeekFrom::Start(offset)).await?;
                    file.write_all(buffer.as_ref()).await?;
                }
            }

            file.sync_all().await?;

            // Clear dirty set
            {
                let mut dirty = self.dirty.write().await;
                for id in dirty_pages {
                    dirty.remove(&id);
                }
            }

            Ok(())
        }
    }

    async fn get_next_lpage_id(&self) -> Result<LPageId> {
        let buffer = self.read_page(0).await?;
        let header = DirectoryFileHeaderPage::new(buffer.as_ref());
        let data = header.data().map_err(|e| Error::PageCorruption(format!("{:?}", e)))?;
        Ok(data.next_lpage_id)
    }

    async fn update_next_lpage_id(&self, lpage_id: LPageId) -> Result<()> {
        let mut buffer = *self.read_page(0).await?;
        {
            let mut header = DirectoryFileHeaderPage::new(&mut buffer);
            let data = header.data_mut().map_err(|e| Error::PageCorruption(format!("{:?}", e)))?;
            data.next_lpage_id = lpage_id;
        }
        self.write_page(0, Box::new(buffer)).await;
        Ok(())
    }
}
