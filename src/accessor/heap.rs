use std::sync::Arc;

use futures::stream::Stream;

use crate::{
    buffer::BufferPool,
    common::{
        aliases::{self, FileId, PPageId, RecordId},
        constants::PAGE_BUF_SIZE,
        txn::Txn,
    },
    page::overlays::{
        file_header::HeapFileHeaderPage,
        table::HeapPage,
    },
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
struct HeapScanState<B: BufferPool> {
    bp: Arc<B>,
    file_id: FileId,
    num_pages: u32,
    current_page: u32,
    buffered: Vec<(Vec<u8>, RecordId)>,
    buf_idx: usize,
    txn: Txn,
}

/// Perform a sequential scan over all visible tuples in a heap file.
///
/// Returns a lazy async stream that fetches one page at a time from the
/// buffer pool, extracts visible tuples, and yields them with their RecordIds.
/// Each item is wrapped in `Result` to propagate I/O and page corruption errors.
pub(super) async fn scan<B: BufferPool>(
    bp: Arc<B>,
    file_id: FileId,
    txn: Txn,
) -> Result<impl Stream<Item = Result<(Vec<u8>, RecordId)>> + Send> {
    // Read file header to get page count
    let header_loc = PPageId { file: file_id, offset: 0 };
    let guard = bp
        .fetch_page_at_loc_read(header_loc)
        .await
        .map_err(Error::BufferError)?;
    let header_page = HeapFileHeaderPage::new(&*guard);
    let num_pages = header_page
        .data()
        .map_err(|_| Error::PageCorruption("heap file header unreadable".into()))?
        .num_pages;
    drop(guard);

    let state = HeapScanState {
        bp,
        file_id,
        num_pages,
        current_page: 0, // incremented to 1 on first iteration
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

            // Advance to next page
            state.current_page += 1;
            if state.current_page > state.num_pages {
                return None;
            }

            // Clone Arc to avoid borrowing state across the guard's lifetime
            let bp = Arc::clone(&state.bp);
            let loc = PPageId {
                file: state.file_id,
                offset: state.current_page as u64 * PAGE_BUF_SIZE as u64,
            };
            let guard = match bp.fetch_page_at_loc_read(loc).await {
                Ok(g) => g,
                Err(e) => return Some((Err(Error::BufferError(e)), state)),
            };
            let page = HeapPage::new(&*guard);

            state.buffered.clear();
            state.buf_idx = 0;

            let num_slots = match page.header() {
                Ok(h) => h.num_slots,
                Err(_) => {
                    return Some((
                        Err(Error::PageCorruption(format!(
                            "heap page {} header unreadable",
                            state.current_page
                        ))),
                        state,
                    ));
                }
            };

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
                        page_id: state.current_page,
                        slot_id: slot,
                    };
                    state.buffered.push((user_data, rid));
                }
            }
            // Loop back to yield from buffer
        }
    }))
}

// ============================================================================
// TABLE INSERT
// ============================================================================

/// Insert a tuple into a heap file.
///
/// Prepends the MVCC header (XMIN = txn.id, XMAX = 0), then scans existing
/// pages for free space. If none found, extends the file with a new page.
///
/// When allocating a new page, the file header is updated first (under a write
/// latch) before the data page is written. This ensures a crash between the two
/// operations leaves the header pointing at the new page (which will be empty
/// on recovery) rather than the reverse (data written, header stale, page
/// number reused on next insert).
pub(super) async fn insert<B: BufferPool>(
    bp: &B,
    file_id: FileId,
    txn: &Txn,
    tuple: &[u8],
) -> Result<RecordId> {
    // Build the full on-disk tuple: [XMIN | XMAX | user_data]
    let header = visibility::make_header(txn.id);
    let mut full_tuple = Vec::with_capacity(visibility::TUPLE_HEADER_SIZE + tuple.len());
    full_tuple.extend_from_slice(&header);
    full_tuple.extend_from_slice(tuple);

    // Read file header for page count
    let header_loc = PPageId { file: file_id, offset: 0 };
    let guard = bp
        .fetch_page_at_loc_read(header_loc)
        .await
        .map_err(Error::BufferError)?;
    let header_page = HeapFileHeaderPage::new(&*guard);
    let num_pages = header_page
        .data()
        .map_err(|_| Error::PageCorruption("heap file header unreadable".into()))?
        .num_pages;
    drop(guard);

    // Try inserting into existing pages (linear scan for free space)
    for page_num in 1..=num_pages {
        let loc = PPageId {
            file: file_id,
            offset: page_num as u64 * PAGE_BUF_SIZE as u64,
        };

        let mut guard = bp
            .fetch_page_at_loc_write(loc)
            .await
            .map_err(Error::BufferError)?;
        let mut page = HeapPage::new(&mut *guard);

        match page.insert(&full_tuple) {
            Ok(slot_id) => {
                // TODO: guard.commit_wal(lsn) once WAL is implemented
                return Ok(RecordId {
                    page_id: page_num,
                    slot_id,
                });
            }
            Err(_) => continue, // NoSpace or other error, try next page
        }
    }

    // No space in existing pages — allocate a new page.
    // Update the file header FIRST to prevent crash-window corruption:
    // if we crash after the header update but before the page write, recovery
    // sees an empty page (benign). The reverse would reuse the page number.
    let new_page_num = num_pages
        .checked_add(1)
        .ok_or_else(|| Error::CapacityExceeded("heap file num_pages overflow".into()))?;

    let mut header_guard = bp
        .fetch_page_at_loc_write(header_loc)
        .await
        .map_err(Error::BufferError)?;
    let mut header_page = HeapFileHeaderPage::new(&mut *header_guard);
    header_page
        .data_mut()
        .map_err(|_| Error::PageCorruption("heap file header unwritable".into()))?
        .num_pages = new_page_num;
    // Keep header_guard alive until after page write for atomicity
    // (both writes are under their respective latches).

    let new_loc = PPageId {
        file: file_id,
        offset: new_page_num as u64 * PAGE_BUF_SIZE as u64,
    };

    let mut guard = bp
        .fetch_page_at_loc_write(new_loc)
        .await
        .map_err(Error::BufferError)?;

    // Initialize the new page as an empty heap page
    let mut page = HeapPage::new(&mut *guard);
    page.init()
        .map_err(|_| Error::PageCorruption("failed to init new heap page".into()))?;

    let slot_id = page
        .insert(&full_tuple)
        .map_err(|_| Error::PageCorruption("insert into fresh page failed".into()))?;

    drop(page);
    drop(guard);
    drop(header_guard);

    // TODO: header_guard.commit_wal(lsn)

    Ok(RecordId {
        page_id: new_page_num,
        slot_id,
    })
}

// ============================================================================
// TABLE GET
// ============================================================================

/// Fetch a single tuple by RecordId, checking MVCC visibility.
pub(super) async fn get<B: BufferPool>(
    bp: &B,
    file_id: FileId,
    txn: &Txn,
    rid: aliases::RecordId,
) -> Result<Vec<u8>> {
    let loc = PPageId {
        file: file_id,
        offset: rid.page_id as u64 * PAGE_BUF_SIZE as u64,
    };

    let guard = bp
        .fetch_page_at_loc_read(loc)
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
pub(super) async fn delete<B: BufferPool>(
    bp: &B,
    file_id: FileId,
    txn: &Txn,
    rid: aliases::RecordId,
) -> Result<()> {
    let loc = PPageId {
        file: file_id,
        offset: rid.page_id as u64 * PAGE_BUF_SIZE as u64,
    };

    let mut guard = bp
        .fetch_page_at_loc_write(loc)
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

    // TODO: guard.commit_wal(lsn)

    Ok(())
}
