# Accessor Architecture

## Overview

The Accessor is the data access layer that bridges the executor (logical query operations) with the storage engine (buffer pool + page overlays). It provides tuple-level operations — scan, insert, get, delete — while hiding page boundaries, MVCC visibility, and B-Tree traversal from the layers above.

```
Executor
  │  "give me all visible tuples from table X"
  ▼
Accessor (this layer)
  │  per-page iteration, MVCC checks, B-Tree traversal
  ▼
BufferPool
  │  page fetch/latch, cache management
  ▼
DiskManager
  │  physical I/O
  ▼
Disk
```

## Module Structure

```
src/accessor/
├── mod.rs              # Re-exports
├── accessor.rs         # Accessor trait definition + Error enum
├── accessor_impl.rs    # AccessorImpl<B> — concrete implementation
├── visibility.rs       # MVCC tuple visibility (XMIN/XMAX)
├── heap.rs             # Heap table operations (scan, insert, get, delete)
├── btree.rs            # B-Tree index operations (scan, insert, get, delete)
└── catalog_cache.rs    # In-memory catalog for O(1) metadata lookups
```

## Accessor Trait

The `Accessor` trait defines the contract between the executor and the storage layer:

```rust
pub trait Accessor: Send + Sync + 'static {
    // Table operations (heap file)
    fn table_scan(txn, table_oid)   → Stream<(Vec<u8>, RecordId)>
    fn table_insert(txn, table_oid, tuple) → RecordId
    fn table_get(txn, table_oid, rid)      → Vec<u8>
    fn table_delete(txn, table_oid, rid)   → ()

    // Index operations (B-Tree file)
    fn index_scan(txn, index_oid, start, end) → Stream<(Vec<u8>, RecordId)>
    fn index_insert(txn, index_oid, key, rid) → ()
    fn index_get(txn, index_oid, key)         → RecordId
    fn index_delete(txn, index_oid, key, rid) → ()

    // Catalog operations (synchronous, from cache)
    fn catalog_get_table_by_name(txn, name)  → Table
    fn catalog_get_table_by_oid(txn, oid)    → Table
    fn catalog_get_index_by_name(txn, name)  → Index
    fn catalog_get_index_by_oid(txn, oid)    → Index
    fn catalog_get_table_columns(txn, oid)   → Vec<Column>
}
```

### Key Design Decisions

- **Async table/index ops**: All I/O operations return `impl Future` to support async buffer pool.
- **Sync catalog ops**: Catalog pages are pinned in memory at startup, so lookups are O(1) HashMap access.
- **`Vec<u8>` tuples**: The accessor works with raw bytes. Deserialization to typed `Value`s is the executor's job (via `TupleLayout`).
- **RecordId**: `(page_id: u32, slot_id: u16)` — the accessor uses file-local page numbers, not global logical page IDs.

## AccessorImpl

```rust
pub struct AccessorImpl<B: BufferPool> {
    bp: Arc<B>,
    catalog: RwLock<CatalogCache>,
}
```

- Generic over any `BufferPool` implementation
- `Arc<B>` allows cloning the buffer pool reference into scan streams (which must be `'static`)
- `RwLock<CatalogCache>` provides concurrent read access with exclusive writes during DDL

### Method Dispatch

Each `Accessor` trait method:
1. Looks up `file_id` from the catalog cache (sync, O(1))
2. Delegates to the appropriate module (`heap::*` or `btree::*`)
3. The module interacts with the buffer pool and overlays

## MVCC Visibility (`visibility.rs`)

Every tuple stored on a heap page has a 16-byte header:

```
Offset | Size | Field | Description
-------|------|-------|------------
0      | 8    | XMIN  | Transaction ID that created this tuple
8      | 8    | XMAX  | Transaction ID that deleted this tuple (0 = live)
```

### Visibility Rules

| Isolation Level  | Visible When |
|------------------|-------------|
| ReadUncommitted  | Always |
| ReadCommitted    | XMIN <= txn.id AND (XMAX == 0 OR XMAX > txn.id) |
| Snapshot         | XMIN <= txn.id AND (XMAX == 0 OR XMAX > txn.id) |

### Tuple Data Flow

```
INSERT: executor sends [user_data]
          → accessor prepends [XMIN | XMAX=0]
          → stored on heap page as [XMIN | XMAX | user_data]

SCAN:   accessor reads [XMIN | XMAX | user_data] from heap page
          → checks visibility(XMIN, XMAX, txn)
          → strips header, yields [user_data] to executor

DELETE:  accessor reads tuple, sets XMAX = txn.id (soft delete)
          → tuple remains physically, invisible to future txns
```

## Heap Operations (`heap.rs`)

### Page Addressing

Each table has its own `.heap` file identified by `file_id`:
- Page 0: `HeapFileHeaderPage` (metadata: `num_pages`, `table_oid`, etc.)
- Page 1..N: `HeapPage` (slotted tuple storage)

Pages are addressed as: `PPageId { file: file_id, offset: page_num * 4096 }`

### Sequential Scan

The scan returns a lazy async `Stream` that buffers one page at a time:

```
State machine (futures::stream::unfold):
  ┌─────────────────────────────────┐
  │ 1. Try yield from buffer        │ ──→ yield (user_data, RecordId)
  │ 2. If buffer empty:             │
  │    a. Advance to next page      │
  │    b. Fetch page (read latch)   │
  │    c. Iterate slots             │
  │    d. Filter: skip tombstones   │
  │    e. Filter: MVCC visibility   │
  │    f. Strip XMIN/XMAX header    │
  │    g. Buffer visible tuples     │
  │    h. Release page latch        │
  │ 3. Loop back to step 1          │
  └─────────────────────────────────┘
```

**Why buffer per-page?** The page latch must be released before fetching the next page. We can't hold a reference to page data across yield points. So we copy visible tuples into a Vec, release the latch, then yield from the Vec.

### Insert

1. Build full tuple: `[XMIN | XMAX=0 | user_data]`
2. Read file header → `num_pages`
3. Linear scan existing pages for free space (`HeapPage::insert`)
4. If no space: allocate new page, `HeapPage::init()`, insert, update header's `num_pages`

### Get

1. Compute `PPageId` from `RecordId.page_id`
2. Fetch page (read latch)
3. `HeapPage::get_data(slot_id)` → raw bytes
4. Check MVCC visibility → `TupleNotVisible` error if invisible
5. Strip header, return user data

### Delete

1. Fetch page (write latch)
2. `HeapPage::get_data_mut(slot_id)` → mutable raw bytes
3. Set XMAX = `txn.id` (soft delete)
4. Release write latch

## B-Tree Index Operations (`btree.rs`)

### Page Addressing

Each index has its own `.idx` file:
- Page 0: `IndexFileHeaderPage` (metadata: `root_page`, `num_pages`, etc.)
- Page 1+: `BTreeInnerPage` or `BTreeLeafPage`

### Tree Traversal (`find_leaf`)

```
current = root_page (from file header)

loop:
  fetch page (read latch)
  check page_type via UberPageHeader

  if BTreeInner:
    child = BTreeInnerPage::find_child(key)
    release latch
    current = child

  if BTreeLeaf:
    return current  (this is the target leaf)
```

No latch coupling needed for reads — each page is fetched and released independently.

### Range Scan

```
1. find_leaf(start_key) → leaf page number
2. Buffer all entries from leaf where key >= start and key <= end
3. Follow next_page sibling pointer to next leaf
4. Repeat until key > end or no more siblings
5. Yield entries as (key_bytes, RecordId)
```

### Point Lookup

```
1. find_leaf(key) → leaf page number
2. BTreeLeafPage::lookup(key) → Option<(LPageId, SlotId)>
3. Return RecordId or TupleNonExistent error
```

### Insert (with split propagation)

```
1. read_root_page() — if root == 0, create root leaf, insert, set root, done
2. find_leaf_with_path(key) → (leaf_num, path = [root, ..., parent])
3. Acquire write latch on leaf
4. If leaf is full (num_entries >= MAX_ENTRIES):
     split_leaf(extra_entry) → (new_leaf, separator)
     insert_into_ancestors(path, separator, new_leaf)
5. Else:
     BTreeLeafPage::insert_entry(key, page_id, slot_id)
     If needs_split() after insert:
       split_leaf(None) → (new_leaf, separator)
       insert_into_ancestors(path, separator, new_leaf)
```

#### Leaf Split (`split_leaf`)

```
1. Read all entries from leaf into Vec
2. Merge extra_entry (if any) in sorted position
3. Split at midpoint: entries[..mid] stay, entries[mid..] → new leaf
4. separator = entries[mid].key (first key of new leaf)
5. Allocate new leaf page, init with upper half entries
6. Re-init old leaf with lower half entries
7. Update sibling chain: old.next → new, new.prev → old, new.next → old_next
8. If old_next exists, update old_next.prev → new
```

#### Inner Page Split (`split_inner`)

```
1. Read all separators from inner page into Vec
2. Insert new separator in sorted position
3. Split: entries[..mid] stay, entries[mid] pushed up, entries[mid+1..] → new page
4. New inner page's leftmost_child = entries[mid].right_child
5. Allocate new inner page, init with upper half
6. Re-init old inner page with lower half
```

#### Cascading Split (`insert_into_ancestors`)

```
for each parent in path (bottom-up):
  Write-latch parent
  If parent has room:
    insert_separator(sep_key, new_child) → done
  Else:
    split_inner(sep_key, new_child) → (new_inner, pushed_up_key)
    Continue with pushed_up_key → grandparent

If path exhausted (root was split or leaf was root):
  Allocate new root inner page
  leftmost_child = old_root, insert separator → new_child
  Update file header root_page = new_root
```

### Delete (with leaf merge)

```
1. read_root_page() — if root == 0, return TupleNonExistent
2. find_leaf_with_path(key) → (leaf_num, path)
3. Acquire write latch on leaf
4. BTreeLeafPage::delete_by_key(key)
5. If can_merge(): try_merge_leaf(leaf_num, path)
```

#### Leaf Merge (`try_merge_leaf`)

```
1. Read parent to find leaf's position and merge candidate
2. Determine survivor (left) and removed (right) pages:
   - If leaf is leftmost_child: survivor = leaf, removed = right sibling
   - Otherwise: survivor = left sibling, removed = leaf
3. Check feasibility: survivor_entries + removed_entries <= MAX_ENTRIES
4. Move all entries from removed into survivor
5. Update sibling chain: survivor.next = removed.next
6. If removed.next exists: removed.next.prev = survivor
7. Remove separator from parent (also removes removed's child pointer)
8. If parent is root and now has 0 keys:
   Promote root's leftmost_child as new root (tree shrinks by 1 level)
```

Note: inner page merges are not cascaded. Underfull inner pages remain
in the tree but are functionally correct.

## Catalog Cache (`catalog_cache.rs`)

```rust
struct CatalogCache {
    tables:         HashMap<OId, Table>,
    tables_by_name: HashMap<String, OId>,
    columns:        HashMap<OId, Vec<Column>>,   // table_oid → columns
    indexes:        HashMap<OId, Index>,
    indexes_by_name: HashMap<String, OId>,
}
```

- Populated at startup with system table definitions (`sys_tables`, `sys_columns`, `sys_indexes`)
- Extended by DDL operations (`register_table`, `register_index`)
- All lookups are O(1) HashMap access
- Protected by `RwLock` — reads are concurrent, writes (DDL) are exclusive

## Error Handling

```rust
pub enum Error {
    BufferError(buffer::Error),     // I/O or latch errors
    TupleNonExistent,               // RecordId points to nothing
    TupleNotVisible(TxnId, TxnId, TxnId),  // MVCC violation (op_txn, xmin, xmax)
    NotFound(String),               // Catalog lookup failed
    PageCorruption(String),         // Overlay parse failure
}
```

## Responsibility Boundaries

| Concern | Accessor | Executor | Overlay |
|---------|:--------:|:--------:|:-------:|
| Page fetch / latch | x | | |
| MVCC visibility check | x | | |
| B-Tree traversal | x | | |
| B-Tree split / merge | x | | |
| Free space management | x | | |
| Tuple serialization | | x | |
| Predicate evaluation | | | |
| Join / Sort / Limit | | x | |
| Read/write entries within one page | | | x |
| Binary search within one page | | | x |
| Page capacity tracking | | | x |

## Future Work

- **Latch crabbing for writes**: Currently, split propagation releases the leaf latch before acquiring parent latches (brief inconsistency window). Use `is_safe_for_insert()` during traversal to release parent latches early, eliminating the window. Consider B-link tree (high keys + right-link pointers) for lock-free readers during splits.
- **Inner page merge on delete**: Leaf merges are implemented but inner page underflow is not cascaded. Implement inner page merge with separator pull-down and recursive parent shrinking.
- **WAL integration**: Every write through a `PageWriteGuard` should call `commit_wal(lsn)` after modification.
- **Free space map**: Replace linear page scan in `heap::insert` with a free space bucket lookup via the page directory.
- **Bulk loading**: Sorted insert path for initial index population.
- **Page compaction**: Reclaim space from soft-deleted tuples on heap pages.
