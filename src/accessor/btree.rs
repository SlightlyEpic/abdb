use futures::stream::Stream;

use zerocopy::FromBytes;

use crate::{
    buffer::{BufferPool, PageWriteGuard},
    common::{
        aliases::{FileId, LPageId, PPageId, PageBuffer, RecordId},
        txn::Txn,
    },
    page::{
        PageType, UberPageHeader,
        overlays::{
            file_header::IndexFileHeaderPage,
            index::{
                BTreeInnerEntry, BTreeInnerPage, BTreeLeafEntry, BTreeLeafPage, INNER_MAX_KEYS,
                LEAF_MAX_ENTRIES,
            },
        },
    },
};

use super::accessor::{Error, Result};

// ============================================================================
// KEY CONVERSION
// ============================================================================

/// Convert a variable-length key byte slice to a u64 for B-Tree comparison.
///
/// Pads or truncates to 8 bytes, interprets as little-endian u64.
#[inline]
fn key_from_bytes(bytes: &[u8]) -> u64 {
    let mut arr = [0u8; 8];
    let len = bytes.len().min(8);
    arr[..len].copy_from_slice(&bytes[..len]);
    u64::from_le_bytes(arr)
}

/// Convert a u64 key to an 8-byte little-endian Vec for the stream output.
#[inline]
fn key_to_bytes(key: u64) -> Vec<u8> {
    key.to_le_bytes().to_vec()
}

/// Get the PPageId for the file header (always at offset 0).
#[inline]
fn header_loc(file_id: FileId) -> PPageId {
    PPageId {
        file: file_id,
        offset: 0,
    }
}

// ============================================================================
// TREE TRAVERSAL
// ============================================================================

/// Traverse the B-Tree from root to the leaf page containing `key`.
///
/// Uses read latches only. Releases each page latch before fetching the next
/// (no latch coupling needed for pure reads).
async fn find_leaf<B: BufferPool>(
    bp: &'_ B,
    root_page: LPageId,
    key: u64,
) -> Result<LPageId> {
    let mut current = root_page;

    loop {
        let guard = bp
            .fetch_page_read(current)
            .await
            .map_err(Error::BufferError)?;

        let uber = UberPageHeader::read_from_prefix(&*guard)
            .map_err(|_| Error::PageCorruption("cannot read UberPageHeader".into()))?
            .0;
        let page_type = PageType::try_from(uber.page_type_id).map_err(|_| {
            Error::PageCorruption(format!("invalid page type {}", uber.page_type_id))
        })?;

        match page_type {
            PageType::BTreeInner => {
                let page = BTreeInnerPage::new(&*guard);
                current = page.find_child(key);
                // guard dropped here — latch released before next fetch
            }
            PageType::BTreeLeaf => {
                return Ok(current);
            }
            _ => {
                return Err(Error::PageCorruption(format!(
                    "unexpected page type {:?} during B-Tree traversal",
                    page_type
                )));
            }
        }
    }
}

/// Traverse from root to leaf, recording the path of inner pages visited.
///
/// Returns `(leaf_page_num, path)` where `path` is the stack of inner page
/// numbers from root to the parent of the leaf: `[root, ..., parent]`.
async fn find_leaf_with_path<B: BufferPool>(
    bp: &'_ B,
    root_page: LPageId,
    key: u64,
) -> Result<(LPageId, Vec<LPageId>)> {
    let mut current = root_page;
    let mut path = Vec::new();

    loop {
        let guard = bp
            .fetch_page_read(current)
            .await
            .map_err(Error::BufferError)?;

        let uber = UberPageHeader::read_from_prefix(&*guard)
            .map_err(|_| Error::PageCorruption("cannot read UberPageHeader".into()))?
            .0;
        let page_type = PageType::try_from(uber.page_type_id).map_err(|_| {
            Error::PageCorruption(format!("invalid page type {}", uber.page_type_id))
        })?;

        match page_type {
            PageType::BTreeInner => {
                path.push(current);
                let page = BTreeInnerPage::new(&*guard);
                current = page.find_child(key);
            }
            PageType::BTreeLeaf => {
                return Ok((current, path));
            }
            _ => {
                return Err(Error::PageCorruption(format!(
                    "unexpected page type {:?} during B-Tree traversal",
                    page_type
                )));
            }
        }
    }
}

// ============================================================================
// PAGE ALLOCATION & FILE HEADER HELPERS
// ============================================================================

/// Allocate a new page in the index file via the buffer pool.
///
/// Uses `bp.new_page()` which allocates an LPageId, registers it in the
/// page directory, and returns a write guard for the new page.
async fn alloc_page<B: BufferPool>(bp: &'_ B, file_id: FileId) -> Result<(LPageId, B::WriteGuard<'_>)> {
    let guard = bp.new_page(file_id).await.map_err(Error::BufferError)?;
    let lpage_id = guard.lpage_id();
    Ok((lpage_id, guard))
}

/// Update the root_page pointer in the index file header.
async fn set_root_page<B: BufferPool>(bp: &'_ B, file_id: FileId, new_root: LPageId) -> Result<()> {
    let mut guard = bp
        .fetch_page_at_loc_write(header_loc(file_id))
        .await
        .map_err(Error::BufferError)?;
    let mut header = IndexFileHeaderPage::new(&mut *guard);
    let data = header
        .data_mut()
        .map_err(|_| Error::PageCorruption("index file header unwritable".into()))?;
    data.root_page = new_root;
    guard.mark_dirty().map_err(Error::BufferError)?;
    Ok(())
}

/// Atomically allocate a new page and set it as root.
///
/// Allocates a new page via buffer pool, then updates the root pointer in the
/// header while holding both latches. This prevents the race where two
/// concurrent inserts both exhaust their ancestor locks, each try to create
/// a new root, and the second write silently orphans the first new root.
///
/// Returns `(new_root_lpage_id, old_root_lpage_id, new_root_guard)`.
async fn alloc_and_set_root<B: BufferPool>(
    bp: &'_ B,
    file_id: FileId,
) -> Result<(LPageId, LPageId, B::WriteGuard<'_>)> {
    // Allocate new root page first
    let new_guard = bp.new_page(file_id).await.map_err(Error::BufferError)?;
    let new_root = new_guard.lpage_id();

    // Update header to point to new root
    let mut header_guard = bp
        .fetch_page_at_loc_write(header_loc(file_id))
        .await
        .map_err(Error::BufferError)?;
    let mut header = IndexFileHeaderPage::new(&mut *header_guard);
    let data = header
        .data_mut()
        .map_err(|_| Error::PageCorruption("index file header unwritable".into()))?;
    let old_root = data.root_page;
    data.root_page = new_root;
    header_guard.mark_dirty().map_err(Error::BufferError)?;
    drop(header_guard);

    Ok((new_root, old_root, new_guard))
}

/// Read the root page number from the index file header.
async fn read_root_page<B: BufferPool>(bp: &'_ B, file_id: FileId) -> Result<LPageId> {
    let guard = bp
        .fetch_page_at_loc_read(header_loc(file_id))
        .await
        .map_err(Error::BufferError)?;
    let header = IndexFileHeaderPage::new(&*guard);
    let root_page = header
        .data()
        .map_err(|_| Error::PageCorruption("index file header unreadable".into()))?
        .root_page;
    Ok(root_page)
}

// ============================================================================
// LEAF SPLIT
// ============================================================================

/// Split a leaf page, optionally inserting a new entry before splitting.
///
/// Reads all entries from the leaf, merges in `extra_entry` if provided,
/// then splits at midpoint. Lower half stays in the old leaf, upper half
/// goes to a new leaf. Sibling pointers are updated accordingly.
///
/// Returns `(new_leaf_page_num, separator_key)`.
async fn split_leaf<B: BufferPool>(
    bp: &'_ B,
    file_id: FileId,
    leaf_buf: &mut PageBuffer,
    leaf_page_num: LPageId,
    extra_entry: Option<BTreeLeafEntry>,
) -> Result<(LPageId, u64)> {
    // Read phase — collect entries and sibling pointers
    let (mut entries, old_prev, old_next) = {
        let page = BTreeLeafPage::new(&*leaf_buf);
        let num = page.num_entries();
        let old_prev = page.prev_page();
        let old_next = page.next_page();
        let mut entries = Vec::with_capacity(num as usize + 1);
        for i in 0..num {
            entries.push(
                page.entry(i)
                    .map_err(|e| Error::PageCorruption(format!("read leaf entry: {:?}", e)))?,
            );
        }
        (entries, old_prev, old_next)
    };

    // Merge extra entry in sorted position
    if let Some(entry) = extra_entry {
        let pos = entries
            .binary_search_by_key(&entry.key, |e| e.key)
            .err()
            .ok_or_else(|| Error::PageCorruption("duplicate key during leaf split".into()))?;
        entries.insert(pos, entry);
    }

    // Split point — separator is the first key of the upper half
    let mid = entries.len() / 2;
    let separator = entries[mid].key;

    // Allocate and initialize new leaf with upper half
    let (new_leaf_num, mut new_guard) = alloc_page(bp, file_id).await?;
    {
        let mut new_page = BTreeLeafPage::init(&mut *new_guard, new_leaf_num);
        for entry in &entries[mid..] {
            new_page
                .insert_entry(entry.key, entry.record_page, entry.record_slot)
                .map_err(|e| Error::PageCorruption(format!("split new leaf: {:?}", e)))?;
        }
        new_page.set_next_page(old_next);
        new_page.set_prev_page(leaf_page_num);
    }
    new_guard.mark_dirty().map_err(Error::BufferError)?;
    drop(new_guard);

    // Rebuild old leaf with lower half (re-init preserves page_id)
    {
        let mut old_page = BTreeLeafPage::init(&mut *leaf_buf, leaf_page_num);
        for entry in &entries[..mid] {
            old_page
                .insert_entry(entry.key, entry.record_page, entry.record_slot)
                .map_err(|e| Error::PageCorruption(format!("split old leaf: {:?}", e)))?;
        }
        old_page.set_next_page(new_leaf_num);
        old_page.set_prev_page(old_prev);
    }

    // Update the old right sibling's prev_page pointer
    if old_next != 0 {
        let mut sib_guard = bp
            .fetch_page_write(old_next)
            .await
            .map_err(Error::BufferError)?;
        let mut sib_page = BTreeLeafPage::new(&mut *sib_guard);
        sib_page.set_prev_page(new_leaf_num);
        sib_guard.mark_dirty().map_err(Error::BufferError)?;
    }

    Ok((new_leaf_num, separator))
}

// ============================================================================
// INNER PAGE SPLIT
// ============================================================================

/// Split a full inner page while inserting a new separator.
///
/// Reads all separators, inserts the new one in sorted position, splits at
/// midpoint. Lower half stays in old page, middle key is pushed up to the
/// parent, upper half goes to a new inner page.
///
/// Returns `(new_inner_page_num, pushed_up_separator_key)`.
async fn split_inner<B: BufferPool>(
    bp: &'_ B,
    file_id: FileId,
    inner_buf: &mut PageBuffer,
    inner_page_num: LPageId,
    sep_key: u64,
    right_child: LPageId,
) -> Result<(LPageId, u64)> {
    // Read phase
    let (mut entries, old_leftmost) = {
        let page = BTreeInnerPage::new(&*inner_buf);
        let num = page.num_keys();
        let old_leftmost = page.leftmost_child();
        let mut entries = Vec::with_capacity(num as usize + 1);
        for i in 0..num {
            entries.push(
                page.entry(i)
                    .map_err(|e| Error::PageCorruption(format!("read inner entry: {:?}", e)))?,
            );
        }
        (entries, old_leftmost)
    };

    // Merge new separator in sorted position
    let pos = entries
        .binary_search_by_key(&sep_key, |e| e.separator_key)
        .err()
        .ok_or_else(|| Error::PageCorruption("duplicate separator during inner split".into()))?;
    entries.insert(pos, BTreeInnerEntry::new(sep_key, right_child));

    // Split: entries[..mid] stay, entries[mid] pushed up, entries[mid+1..] → new page
    let mid = entries.len() / 2;
    let pushed_up_key = entries[mid].separator_key;
    let new_leftmost = entries[mid].right_child;

    // Allocate and initialize new inner page with upper half
    let (new_inner_num, mut new_guard) = alloc_page(bp, file_id).await?;
    {
        let mut new_page = BTreeInnerPage::init(&mut *new_guard, new_inner_num, new_leftmost);
        for entry in &entries[mid + 1..] {
            new_page
                .insert_separator(entry.separator_key, entry.right_child)
                .map_err(|e| Error::PageCorruption(format!("split new inner: {:?}", e)))?;
        }
    }
    new_guard.mark_dirty().map_err(Error::BufferError)?;
    drop(new_guard);

    // Rebuild old inner page with lower half
    {
        let mut old_page = BTreeInnerPage::init(&mut *inner_buf, inner_page_num, old_leftmost);
        for entry in &entries[..mid] {
            old_page
                .insert_separator(entry.separator_key, entry.right_child)
                .map_err(|e| Error::PageCorruption(format!("split old inner: {:?}", e)))?;
        }
    }

    Ok((new_inner_num, pushed_up_key))
}

// ============================================================================
// LATCH-CRABBING WRITE TRAVERSAL
// ============================================================================

/// Traverse the B-Tree with write latches using latch crabbing.
///
/// Acquires write latches top-down. When a node is safe for insert (won't
/// split), all ancestor write latches held above it are released — splits
/// cannot propagate past a safe node, so there is no reason to keep
/// ancestors locked.
///
/// Uses `is_safe_for_insert()` from the page overlay (which uses
/// `SAFE_INSERT_THRESHOLD = MAX - 1`) to ensure consistency between the
/// crabbing logic and the page-level safety predicate.
///
/// Returns `(leaf_page, leaf_guard, locked)` where `locked` is ordered
/// root-to-parent and holds write latches only for nodes that are full
/// (unsafe) and may need to split. If the leaf is safe, `locked` is empty.
async fn find_leaf_for_write<B: BufferPool>(
    bp: &'_ B,
    root_page: LPageId,
    key: u64,
) -> Result<(
    LPageId,
    B::WriteGuard<'_>,
    Vec<(LPageId, B::WriteGuard<'_>)>,
)> {
    let mut current = root_page;
    // Holds write guards for ancestors that are full and may cascade-split.
    let mut locked: Vec<(LPageId, B::WriteGuard<'_>)> = Vec::new();

    loop {
        let guard = bp
            .fetch_page_write(current)
            .await
            .map_err(Error::BufferError)?;

        let uber = UberPageHeader::read_from_prefix(&*guard)
            .map_err(|_| Error::PageCorruption("cannot read UberPageHeader".into()))?
            .0;
        let page_type = PageType::try_from(uber.page_type_id).map_err(|_| {
            Error::PageCorruption(format!("invalid page type {}", uber.page_type_id))
        })?;

        match page_type {
            PageType::BTreeInner => {
                let (is_safe, next_child) = {
                    let page = BTreeInnerPage::new(&*guard);
                    // Use the page overlay's own safety predicate for consistency
                    // (SAFE_INSERT_THRESHOLD = MAX_KEYS - 1).
                    (page.is_safe_for_insert(), page.find_child(key))
                };
                if is_safe {
                    // This node won't split, so no split can propagate above it.
                    // Release all ancestor write latches — they are no longer needed.
                    locked.clear();
                }
                // Keep this node's write latch: it is either unsafe (full) and
                // may split, or it is safe but we must hold it until its child
                // is also confirmed safe (at which point locked.clear() will
                // release it on the next iteration).
                locked.push((current, guard));
                current = next_child;
            }
            PageType::BTreeLeaf => {
                let is_safe = {
                    let page = BTreeLeafPage::new(&*guard);
                    // Use the page overlay's own safety predicate for consistency
                    // (SAFE_INSERT_THRESHOLD = MAX_ENTRIES - 1).
                    page.is_safe_for_insert()
                };
                if is_safe {
                    // Safe leaf: no split will occur, so all ancestor latches
                    // can be released now.
                    locked.clear();
                }
                // Return leaf guard separately; locked contains only ancestor guards.
                return Ok((current, guard, locked));
            }
            _ => {
                return Err(Error::PageCorruption(format!(
                    "unexpected page type {:?} during latch-crabbing traversal",
                    page_type
                )));
            }
        }
    }
}

// ============================================================================
// INSERT INTO ANCESTORS (cascading split propagation with pre-held locks)
// ============================================================================

/// Insert a separator into already write-latched ancestors, cascading splits
/// upward.
///
/// Consumes the pre-held write guards in `locked` (ordered root-to-parent),
/// popping from the end (parent-first) to handle split propagation bottom-up.
/// Each guard is used directly — no re-acquisition of latches is needed,
/// eliminating the race window present in a acquire-on-demand approach.
///
/// If all pre-held ancestors split, a new root is created atomically via
/// `alloc_and_set_root` to prevent concurrent root creation races.
async fn insert_into_ancestors_with_locks<B: BufferPool>(
    bp: &'_ B,
    file_id: FileId,
    mut locked: Vec<(LPageId, B::WriteGuard<'_>)>,
    mut sep_key: u64,
    mut new_child: LPageId,
) -> Result<()> {
    while let Some((parent_num, mut guard)) = locked.pop() {
        let is_full = {
            let page = BTreeInnerPage::new(&*guard);
            page.num_keys() >= INNER_MAX_KEYS
        };

        if !is_full {
            // Parent has room — insert separator and we're done.
            let mut page = BTreeInnerPage::new(&mut *guard);
            page.insert_separator(sep_key, new_child)
                .map_err(|e| Error::PageCorruption(format!("parent insert: {:?}", e)))?;
            return Ok(());
        }

        // Parent is full — split using the pre-held write guard.
        let (new_inner, pushed_up) =
            split_inner(bp, file_id, &mut *guard, parent_num, sep_key, new_child).await?;
        drop(guard);

        sep_key = pushed_up;
        new_child = new_inner;
    }

    // All pre-held ancestors were split (or locked was empty) — create a new
    // root. Use alloc_and_set_root to atomically allocate and update the root
    // pointer under a single file header latch, preventing races where two
    // concurrent inserts both try to create a new root.
    let (new_root_num, old_root, mut root_guard) = alloc_and_set_root(bp, file_id).await?;
    {
        let mut root_page = BTreeInnerPage::init(&mut *root_guard, new_root_num, old_root);
        root_page
            .insert_separator(sep_key, new_child)
            .map_err(|e| Error::PageCorruption(format!("new root insert: {:?}", e)))?;
    }
    root_guard.mark_dirty().map_err(Error::BufferError)?;

    Ok(())
}

// ============================================================================
// INDEX SCAN
// ============================================================================

/// State machine for the B-Tree range scan stream.
struct BTreeScanState<'a, B: BufferPool> {
    bp: &'a B,
    current_leaf: LPageId,
    start_key: Option<u64>,
    end_key: Option<u64>,
    buffered: Vec<(Vec<u8>, RecordId)>,
    buf_idx: usize,
    done: bool,
}

/// Perform a range scan over the B-Tree index.
///
/// Traverses from root to the leaf containing `start_key`, then follows
/// sibling pointers (`next_page`) to scan entries in key order until
/// `end_key` is exceeded or no more leaves exist.
///
/// Each item is wrapped in `Result` to propagate I/O and page errors.
pub(super) async fn scan<B: BufferPool>(
    bp: &'_ B,
    file_id: FileId,
    _txn: Txn,
    start_key: Option<Vec<u8>>,
    end_key: Option<Vec<u8>>,
) -> Result<impl Stream<Item = Result<(Vec<u8>, RecordId)>> + Send> {
    let root_page = read_root_page(bp, file_id).await?;

    let start = start_key.as_deref().map(key_from_bytes);
    let end = end_key.as_deref().map(key_from_bytes);

    // Find starting leaf (or mark done for empty tree)
    let (leaf_page, done) = if root_page == 0 {
        (0, true)
    } else {
        let start_val = start.unwrap_or(0);
        (find_leaf(bp, root_page, start_val).await?, false)
    };

    let state = BTreeScanState {
        bp,
        current_leaf: leaf_page,
        start_key: start,
        end_key: end,
        buffered: Vec::new(),
        buf_idx: 0,
        done,
    };

    Ok(futures::stream::unfold(state, |mut state| async move {
        if state.done {
            return None;
        }

        loop {
            // Yield from buffer
            if state.buf_idx < state.buffered.len() {
                let item = state.buffered[state.buf_idx].clone();
                state.buf_idx += 1;
                return Some((Ok(item), state));
            }

            // Need to load a leaf page
            if state.current_leaf == 0 || state.done {
                return None;
            }

            let bp = state.bp;
            let guard = match bp.fetch_page_read(state.current_leaf).await {
                Ok(g) => g,
                Err(e) => return Some((Err(Error::BufferError(e)), state)),
            };

            let page = BTreeLeafPage::new(&*guard);
            let num_entries = page.num_entries();
            let next_leaf = page.next_page();

            state.buffered.clear();
            state.buf_idx = 0;

            for i in 0..num_entries {
                let entry = match page.entry(i) {
                    Ok(e) => e,
                    Err(e) => {
                        return Some((
                            Err(Error::PageCorruption(format!(
                                "btree leaf entry read: {:?}",
                                e
                            ))),
                            state,
                        ));
                    }
                };

                // Check start bound — skip entries before start_key
                if let Some(start) = state.start_key {
                    if entry.key < start {
                        continue;
                    }
                }

                // Check end bound
                if let Some(end) = state.end_key {
                    if entry.key > end {
                        state.done = true;
                        break;
                    }
                }

                let (page_id, slot_id) = entry.record_id();
                let rid = RecordId { page_id, slot_id };
                state.buffered.push((key_to_bytes(entry.key), rid));
            }

            // After processing the first leaf, clear start_key so subsequent
            // leaves don't re-apply the filter (they are entirely >= start).
            state.start_key = None;

            // Advance to next sibling leaf
            state.current_leaf = next_leaf;
            if next_leaf == 0 {
                // No more siblings — after draining buffer we're done
                if state.buffered.is_empty() {
                    return None;
                }
            }
            // Loop back to yield from buffer
        }
    }))
}

// ============================================================================
// INDEX INSERT
// ============================================================================

/// Insert a key-RecordId pair into the B-Tree index.
///
/// Handles three cases:
/// 1. Empty tree: creates a root leaf page and inserts.
/// 2. Leaf has room: simple insert.
/// 3. Leaf is full: splits the leaf and propagates the separator up the tree,
///    creating a new root if necessary.
pub(super) async fn insert<B: BufferPool>(
    bp: &B,
    file_id: FileId,
    key: &[u8],
    rid: RecordId,
) -> Result<()> {
    let key_u64 = key_from_bytes(key);

    let root_page = read_root_page(bp, file_id).await?;

    // Handle empty tree — create root leaf with the first entry
    if root_page == 0 {
        let (new_page_num, mut guard) = alloc_page(bp, file_id).await?;
        {
            let mut page = BTreeLeafPage::init(&mut *guard, new_page_num);
            page.insert_entry(key_u64, rid.page_id, rid.slot_id)
                .map_err(|e| Error::PageCorruption(format!("leaf insert: {:?}", e)))?;
        }
        guard.mark_dirty().map_err(Error::BufferError)?;

        set_root_page(bp, file_id, new_page_num).await?;
        return Ok(());
    }

    // Latch-crabbing traversal: acquire write latches top-down, releasing safe
    // ancestors early. Returns the leaf's write latch plus write latches for
    // all full (unsafe) ancestors that may need to split.
    let (leaf_num, mut leaf_guard, locked) =
        find_leaf_for_write(bp, root_page, key_u64).await?;

    let needs_split = {
        let page = BTreeLeafPage::new(&*leaf_guard);
        page.needs_split()
    };

    if needs_split {
        // Leaf is full — split with the new entry merged in, then propagate
        // the separator up through the pre-held ancestor write latches.
        let extra = BTreeLeafEntry::new(key_u64, rid.page_id, rid.slot_id);
        let (new_leaf, sep) =
            split_leaf(bp, file_id, &mut *leaf_guard, leaf_num, Some(extra)).await?;
        drop(leaf_guard);

        insert_into_ancestors_with_locks(bp, file_id, locked, sep, new_leaf).await?;
    } else {
        // Leaf has room — simple insert. All ancestor write latches in `locked`
        // are released automatically when `locked` is dropped at end of scope.
        let mut page = BTreeLeafPage::new(&mut *leaf_guard);
        page.insert_entry(key_u64, rid.page_id, rid.slot_id)
            .map_err(|e| Error::PageCorruption(format!("leaf insert: {:?}", e)))?;
    }

    Ok(())
}

// ============================================================================
// INDEX GET (point lookup)
// ============================================================================

/// Look up a single key in the B-Tree index.
///
/// Only valid for unique indexes. Returns the RecordId of the matching tuple.
pub(super) async fn get<B: BufferPool>(bp: &'_ B, file_id: FileId, key: &[u8]) -> Result<RecordId> {
    let key_u64 = key_from_bytes(key);

    let root_page = read_root_page(bp, file_id).await?;

    if root_page == 0 {
        return Err(Error::TupleNonExistent);
    }

    // Find leaf
    let leaf_num = find_leaf(bp, root_page, key_u64).await?;

    let guard = bp
        .fetch_page_read(leaf_num)
        .await
        .map_err(Error::BufferError)?;
    let page = BTreeLeafPage::new(&*guard);

    match page.lookup(key_u64) {
        Some((page_id, slot_id)) => Ok(RecordId { page_id, slot_id }),
        None => Err(Error::TupleNonExistent),
    }
}

// ============================================================================
// LEAF MERGE
// ============================================================================

/// Information about a merge candidate pair.
struct MergeInfo {
    /// The page that survives (receives entries from the removed page).
    survivor: LPageId,
    /// The page whose entries are moved into survivor, then orphaned.
    removed: LPageId,
    /// Index of the separator in the parent to delete after merging.
    /// Deleting this separator also removes the `removed` child pointer.
    sep_idx: u16,
}

/// Find merge candidates for a leaf page in its parent.
///
/// Returns `Some(MergeInfo)` if a sibling exists, or `None` if the leaf
/// is the only child (shouldn't happen in a valid B-Tree with >=2 children).
fn find_merge_candidate(parent_buf: &PageBuffer, leaf_num: LPageId) -> Option<MergeInfo> {
    let parent = BTreeInnerPage::new(&*parent_buf);
    let num_keys = parent.num_keys();

    if num_keys == 0 {
        return None; // Single child — no sibling to merge with
    }

    if leaf_num == parent.leftmost_child() {
        // Leaf is the leftmost child — merge with right sibling
        let right = parent.entry(0).ok()?.right_child;
        Some(MergeInfo {
            survivor: leaf_num,
            removed: right,
            sep_idx: 0,
        })
    } else {
        // Find which entry's right_child points to our leaf
        for i in 0..num_keys {
            let entry = parent.entry(i).ok()?;
            if entry.right_child == leaf_num {
                // Left sibling is the previous child
                let left = if i == 0 {
                    parent.leftmost_child()
                } else {
                    parent.entry(i - 1).ok()?.right_child
                };
                // Merge leaf (right) into left sibling (survivor)
                return Some(MergeInfo {
                    survivor: left,
                    removed: leaf_num,
                    sep_idx: i,
                });
            }
        }
        None // Leaf not found in parent (shouldn't happen)
    }
}

/// Attempt to merge an underflowing leaf with a sibling.
///
/// Uses write latches from the start for the capacity check, eliminating the
/// TOCTOU race where a concurrent insert could grow the survivor between a
/// read-check and write-latch acquisition.
///
/// Merges the `removed` leaf's entries into the `survivor` leaf, updates
/// sibling pointers, and removes the separator from the parent. If the
/// parent is the root and becomes empty (0 keys), the tree shrinks by
/// promoting the root's sole remaining child as the new root.
async fn try_merge_leaf<B: BufferPool>(
    bp: &'_ B,
    file_id: FileId,
    leaf_num: LPageId,
    path: &[LPageId],
    root_page: LPageId,
) -> Result<()> {
    let Some(&parent_num) = path.last() else {
        // Leaf is root — no merge possible
        return Ok(());
    };

    // Write-latch parent to find merge candidates and prevent concurrent
    // modifications to the parent's child list.
    let mut parent_guard = bp
        .fetch_page_write(parent_num)
        .await
        .map_err(Error::BufferError)?;

    let merge_info = match find_merge_candidate(&*parent_guard, leaf_num) {
        Some(info) => info,
        None => return Ok(()),
    };

    // Write-latch survivor and removed under parent latch to prevent TOCTOU
    let mut survivor_guard = bp
        .fetch_page_write(merge_info.survivor)
        .await
        .map_err(Error::BufferError)?;
    let removed_guard = bp
        .fetch_page_write(merge_info.removed)
        .await
        .map_err(Error::BufferError)?;

    let survivor_page = BTreeLeafPage::new(&*survivor_guard);
    let removed_page = BTreeLeafPage::new(&*removed_guard);
    let survivor_count = survivor_page.num_entries();
    let removed_count = removed_page.num_entries();

    if (survivor_count as usize + removed_count as usize) > LEAF_MAX_ENTRIES as usize {
        // Combined entries don't fit — skip merge (still under write latches,
        // so the check is accurate).
        return Ok(());
    }

    // Collect entries from the removed page
    let mut removed_entries = Vec::with_capacity(removed_count as usize);
    for i in 0..removed_count {
        removed_entries.push(
            removed_page
                .entry(i)
                .map_err(|e| Error::PageCorruption(format!("merge read removed: {:?}", e)))?,
        );
    }
    let removed_next = removed_page.next_page();
    drop(removed_guard);

    // Insert removed entries into survivor
    {
        let mut survivor_page = BTreeLeafPage::new(&mut *survivor_guard);
        for entry in &removed_entries {
            survivor_page
                .insert_entry(entry.key, entry.record_page, entry.record_slot)
                .map_err(|e| Error::PageCorruption(format!("merge insert: {:?}", e)))?;
        }
        // Survivor takes over removed's right sibling link
        survivor_page.set_next_page(removed_next);
    }
    survivor_guard.mark_dirty().map_err(Error::BufferError)?;

    // Update removed's right sibling's prev_page to point to survivor
    if removed_next != 0 {
        let mut sib_guard = bp
            .fetch_page_write(removed_next)
            .await
            .map_err(Error::BufferError)?;
        let mut sib_page = BTreeLeafPage::new(&mut *sib_guard);
        sib_page.set_prev_page(merge_info.survivor);
        sib_guard.mark_dirty().map_err(Error::BufferError)?;
    }

    // Remove separator from parent (also removes the removed child pointer)
    {
        let mut parent_page = BTreeInnerPage::new(&mut *parent_guard);
        parent_page
            .delete_separator(merge_info.sep_idx)
            .map_err(|e| Error::PageCorruption(format!("merge delete sep: {:?}", e)))?;
    }
    parent_guard.mark_dirty().map_err(Error::BufferError)?;

    // Check if root should shrink: root inner page with 0 keys means
    // only leftmost_child remains — promote it as the new root
    if parent_num == root_page {
        let parent_page = BTreeInnerPage::new(&*parent_guard);
        if parent_page.num_keys() == 0 {
            let new_root = parent_page.leftmost_child();
            drop(parent_guard);
            set_root_page(bp, file_id, new_root).await?;
            return Ok(());
        }
    }

    Ok(())
}

// ============================================================================
// INDEX DELETE
// ============================================================================

/// Delete a key-RecordId pair from the B-Tree index.
///
/// Finds the leaf containing the key and removes the entry. If the key is
/// not found on the leaf, returns `TupleNonExistent`. If the leaf falls
/// below the merge threshold, attempts to merge it with a sibling and
/// remove the separator from the parent. If the root becomes empty after
/// a merge, the tree shrinks by one level.
///
/// Inner page merges are not cascaded — only leaf merges are performed.
/// Underfull inner pages remain in the tree but are functionally correct.
pub(super) async fn delete<B: BufferPool>(
    bp: &'_ B,
    file_id: FileId,
    key: &[u8],
    rid: RecordId,
) -> Result<()> {
    let key_u64 = key_from_bytes(key);

    let root_page = read_root_page(bp, file_id).await?;

    if root_page == 0 {
        return Err(Error::TupleNonExistent);
    }

    // Use path-tracking traversal for potential merge
    let (leaf_num, path) = find_leaf_with_path(bp, root_page, key_u64).await?;

    let mut guard = bp
        .fetch_page_write(leaf_num)
        .await
        .map_err(Error::BufferError)?;

    let can_merge = {
        let mut page = BTreeLeafPage::new(&mut *guard);

        // First verify the key exists on this leaf before deleting.
        let (found, pos) = page.find_slot(key_u64);
        if !found {
            return Err(Error::TupleNonExistent);
        }

        // Verify the entry matches the expected RecordId. For unique indexes
        // this is always true, but for non-unique indexes this ensures we
        // delete the correct entry rather than an arbitrary one with the same key.
        let entry = page
            .entry(pos)
            .map_err(|e| Error::PageCorruption(format!("leaf entry read: {:?}", e)))?;
        if entry.record_page != rid.page_id || entry.record_slot != rid.slot_id {
            return Err(Error::TupleNonExistent);
        }

        page.delete_entry(pos)
            .map_err(|e| Error::PageCorruption(format!("leaf delete failed: {:?}", e)))?;
        page.can_merge()
    };
    guard.mark_dirty().map_err(Error::BufferError)?;

    if can_merge {
        try_merge_leaf(bp, file_id, leaf_num, &path, root_page).await?;
    }

    Ok(())
}
