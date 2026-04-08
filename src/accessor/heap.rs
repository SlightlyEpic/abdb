use futures::stream::Stream;

use crate::{
    buffer::{BufferPool, PageWriteGuard},
    common::{
        aliases::{self, FileId, LPageId, PPageId, RecordId},
        txn::Txn,
    },
    page::overlays::{file_header::HeapFileHeaderPage, table::HeapPage},
};

use super::{
    accessor::{Error, Result},
    visibility,
};

// ============================================================================
// TABLE SCAN
// ============================================================================

/// State machine for the heap scan stream.
/// Buffers one page's worth of visible tuples at a time.
/// Traverses the heap as a linked list of pages.
struct HeapScanState<'a, B: BufferPool> {
    bp: &'a B,
    /// Current page in the linked list (0 = end of list)
    current_page: LPageId,
    buffered: Vec<(Vec<u8>, RecordId)>,
    buf_idx: usize,
    txn: Txn,
}

/// Perform a sequential scan over all visible tuples in a heap file.
///
/// Returns a lazy async stream that fetches one page at a time from the
/// buffer pool, extracts visible tuples, and yields them with their RecordIds.
/// Each item is wrapped in `Result` to propagate I/O and page corruption errors.
///
/// Pages are traversed as a linked list via next_page pointers.
pub async fn scan<'a, B: BufferPool>(
    bp: &'a B,
    file_id: FileId,
    txn: Txn,
) -> Result<impl Stream<Item = Result<(Vec<u8>, RecordId)>> + Send> {
    // Read file header to get first page
    let header_loc = PPageId {
        file: file_id,
        offset: 0,
    };
    let guard = bp
        .fetch_page_at_loc_read(header_loc)
        .await
        .map_err(Error::BufferError)?;
    let header_page = HeapFileHeaderPage::new(&*guard);
    let first_page = header_page
        .data()
        .map_err(|_| Error::PageCorruption("heap file header unreadable".into()))?
        .first_page;
    drop(guard);

    let state = HeapScanState {
        bp,
        current_page: first_page,
        buffered: Vec::new(),
        buf_idx: 0,
        txn,
    };

    Ok(futures::stream::unfold(state, |mut state| async move {
        loop {
            // Yield from buffer first
            if state.buf_idx < state.buffered.len() {
                let item = state.buffered[state.buf_idx].clone();
                state.buf_idx += 1;
                return Some((Ok(item), state));
            }

            // Check if we've reached the end of the list
            if state.current_page == 0 {
                return None;
            }

            let bp = state.bp;
            let current_lpage_id = state.current_page;

            // Fetch the current page by LPageId
            let guard = match bp.fetch_page_read(current_lpage_id).await {
                Ok(g) => g,
                Err(e) => return Some((Err(Error::BufferError(e)), state)),
            };
            let page = HeapPage::new(&*guard);

            state.buffered.clear();
            state.buf_idx = 0;

            let header = match page.header() {
                Ok(h) => h,
                Err(_) => {
                    return Some((
                        Err(Error::PageCorruption(format!(
                            "heap page {} header unreadable",
                            current_lpage_id
                        ))),
                        state,
                    ));
                }
            };

            let num_slots = header.num_slots;
            let next_page = header.next_page;

            for slot in 0..num_slots {
                let data = match page.get_data(slot as usize) {
                    Ok(d) => d,
                    Err(_) => continue,
                };

                // Skip tombstoned slots (length == 0)
                if data.is_empty() {
                    continue;
                }

                // Check MVCC visibility
                if visibility::is_visible(data, &state.txn) {
                    // Strip the XMIN/XMAX header, return user data only
                    let user_data = data[visibility::TUPLE_HEADER_SIZE..].to_vec();
                    let rid = RecordId {
                        page_id: current_lpage_id,
                        slot_id: slot,
                    };
                    state.buffered.push((user_data, rid));
                }
            }

            // Advance to next page in the list
            state.current_page = next_page;
            // Loop back to yield from buffer
        }
    }))
}

/// Walk every live tuple in a heap file and return the maximum `xmin`.
/// Used at startup to reconstruct the transaction-manager high watermark.
pub async fn max_xmin<B: BufferPool>(
    bp: &B,
    file_id: FileId,
) -> Result<crate::common::aliases::TxnId> {
    let header_loc = PPageId { file: file_id, offset: 0 };
    let guard = bp
        .fetch_page_at_loc_read(header_loc)
        .await
        .map_err(Error::BufferError)?;
    let header_page = HeapFileHeaderPage::new(&*guard);
    let mut current_page = header_page
        .data()
        .map_err(|_| Error::PageCorruption("heap file header unreadable".into()))?
        .first_page;
    drop(guard);

    let mut max: crate::common::aliases::TxnId = 0;
    while current_page != 0 {
        let guard = bp
            .fetch_page_read(current_page)
            .await
            .map_err(Error::BufferError)?;
        let page = HeapPage::new(&*guard);
        let header = page
            .header()
            .map_err(|_| Error::PageCorruption("heap page header unreadable".into()))?;
        let num_slots = header.num_slots;
        let next_page = header.next_page;
        for slot in 0..num_slots {
            let data = match page.get_data(slot as usize) {
                Ok(d) => d,
                Err(_) => continue,
            };
            if data.len() < visibility::TUPLE_HEADER_SIZE {
                continue;
            }
            let xmin = visibility::read_xmin(data);
            if xmin > max {
                max = xmin;
            }
        }
        drop(guard);
        current_page = next_page;
    }
    Ok(max)
}

// ============================================================================
// TABLE INSERT
// ============================================================================

/// Insert a tuple into a heap file.
///
/// Prepends the MVCC header (XMIN = txn.id, XMAX = 0), then traverses the
/// linked list of pages looking for free space. If none found, allocates a
/// new page and prepends it to the list.
///
/// New pages are prepended to the head of the list for efficiency (only need
/// to update header.first_page and new_page.next_page).
pub async fn insert<'a, B: BufferPool>(
    bp: &'a B,
    file_id: FileId,
    txn: &Txn,
    tuple: &[u8],
) -> Result<RecordId> {
    // Build the full on-disk tuple: [XMIN | XMAX | user_data]
    let header = visibility::make_header(txn.id);
    let mut full_tuple = Vec::with_capacity(visibility::TUPLE_HEADER_SIZE + tuple.len());
    full_tuple.extend_from_slice(&header);
    full_tuple.extend_from_slice(tuple);

    // Read file header to get first page
    let header_loc = PPageId {
        file: file_id,
        offset: 0,
    };
    let guard = bp
        .fetch_page_at_loc_read(header_loc)
        .await
        .map_err(Error::BufferError)?;
    let header_page = HeapFileHeaderPage::new(&*guard);
    let first_page = header_page
        .data()
        .map_err(|_| Error::PageCorruption("heap file header unreadable".into()))?
        .first_page;
    drop(guard);

    // Traverse the linked list looking for free space
    let mut current_page = first_page;
    while current_page != 0 {
        let mut guard = bp
            .fetch_page_write(current_page)
            .await
            .map_err(Error::BufferError)?;
        let mut page = HeapPage::new(&mut *guard);

        match page.insert(&full_tuple) {
            Ok(slot_id) => {
                guard.mark_dirty().map_err(Error::BufferError)?;
                // TODO: guard.commit_wal(lsn) once WAL is implemented
                return Ok(RecordId {
                    page_id: current_page,
                    slot_id,
                });
            }
            Err(_) => {
                // NoSpace or other error, get next page and try it
                let next = page.header().map(|h| h.next_page).unwrap_or(0);
                drop(page);
                drop(guard);
                current_page = next;
            }
        }
    }

    // No space in existing pages — allocate a new page via buffer pool
    let mut new_guard = bp.new_page(file_id).await.map_err(Error::BufferError)?;
    let new_lpage_id = new_guard.lpage_id();

    // Initialize the new page as an empty heap page
    {
        let mut page = HeapPage::new(&mut *new_guard);
        page.init()
            .map_err(|_| Error::PageCorruption("failed to init new heap page".into()))?;

        // Link new page to old first_page (prepend to list)
        page.header_mut()
            .map_err(|_| Error::PageCorruption("failed to access new heap page header".into()))?
            .next_page = first_page;

        let slot_id = page
            .insert(&full_tuple)
            .map_err(|_| Error::PageCorruption("insert into fresh page failed".into()))?;

        new_guard.mark_dirty().map_err(Error::BufferError)?;

        // Now update the file header to point to the new page (prepend)
        let mut header_guard = bp
            .fetch_page_at_loc_write(header_loc)
            .await
            .map_err(Error::BufferError)?;
        let mut header_page = HeapFileHeaderPage::new(&mut *header_guard);
        header_page
            .data_mut()
            .map_err(|_| Error::PageCorruption("heap file header unwritable".into()))?
            .first_page = new_lpage_id;
        header_guard.mark_dirty().map_err(Error::BufferError)?;

        // TODO: header_guard.commit_wal(lsn)

        Ok(RecordId {
            page_id: new_lpage_id,
            slot_id,
        })
    }
}

// ============================================================================
// TABLE GET
// ============================================================================

/// Fetch a single tuple by RecordId, checking MVCC visibility.
///
/// The RecordId.page_id is a global LPageId, so we fetch directly by that ID.
pub async fn get<'a, B: BufferPool>(
    bp: &'a B,
    _file_id: FileId,
    txn: &Txn,
    rid: aliases::RecordId,
) -> Result<Vec<u8>> {
    // RecordId.page_id is a global LPageId - fetch directly
    let guard = bp
        .fetch_page_read(rid.page_id)
        .await
        .map_err(Error::BufferError)?;
    let page = HeapPage::new(&*guard);

    let data = page
        .get_data(rid.slot_id as usize)
        .map_err(|_| Error::TupleNonExistent)?;

    if data.is_empty() || data.len() < visibility::TUPLE_HEADER_SIZE {
        return Err(Error::TupleNonExistent);
    }

    if !visibility::is_visible(data, txn) {
        let xmin = visibility::read_xmin(data);
        let xmax = visibility::read_xmax(data);
        return Err(Error::TupleNotVisible(txn.id, xmin, xmax));
    }

    Ok(data[visibility::TUPLE_HEADER_SIZE..].to_vec())
}

// ============================================================================
// TABLE DELETE
// ============================================================================

/// Soft-delete a tuple by setting its XMAX to the current transaction ID.
///
/// The tuple remains physically on the page but becomes invisible to
/// transactions with id >= txn.id.
///
/// Checks visibility before deleting — a transaction cannot delete a tuple
/// it cannot see, and cannot double-delete a tuple already marked deleted.
///
/// The RecordId.page_id is a global LPageId, so we fetch directly by that ID.
pub async fn delete<'a, B: BufferPool>(
    bp: &'a B,
    _file_id: FileId,
    txn: &Txn,
    rid: aliases::RecordId,
) -> Result<()> {
    // RecordId.page_id is a global LPageId - fetch directly
    let mut guard = bp
        .fetch_page_write(rid.page_id)
        .await
        .map_err(Error::BufferError)?;
    let mut page = HeapPage::new(&mut *guard);

    let data = page
        .get_data_mut(rid.slot_id as usize)
        .map_err(|_| Error::TupleNonExistent)?;

    if data.is_empty() || data.len() < visibility::TUPLE_HEADER_SIZE {
        return Err(Error::TupleNonExistent);
    }

    // Check that the tuple is visible to this transaction before deleting.
    // This prevents:
    // - Double-deleting a tuple already deleted by another transaction
    // - Deleting a tuple created by a future (uncommitted) transaction
    if !visibility::is_visible(data, txn) {
        let xmin = visibility::read_xmin(data);
        let xmax = visibility::read_xmax(data);
        return Err(Error::TupleNotVisible(txn.id, xmin, xmax));
    }

    // Check if already deleted by another concurrent transaction
    let existing_xmax = visibility::read_xmax(data);
    if existing_xmax != 0 {
        return Err(Error::AlreadyDeleted(existing_xmax));
    }

    // Set XMAX to mark as deleted for this transaction
    visibility::write_xmax(data, txn.id);

    guard.mark_dirty().map_err(Error::BufferError)?;
    // TODO: guard.commit_wal(lsn)

    Ok(())
}
