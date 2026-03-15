# Overlay Architecture

## Overview

Overlays are type-safe wrappers around raw 4096-byte page buffers. They provide structured, zero-copy access to page data without requiring the buffer itself to be aligned. Every page in the database passes through an overlay to be read or written.

## Design Principles

### Generic Buffer Wrapping

All overlays are generic over `T` with trait bounds:
- `T: AsRef<[u8]>` — read-only access
- `T: AsRef<[u8]> + AsMut<[u8]>` — read-write access

This allows overlays to wrap any buffer type: stack arrays, `Vec<u8>`, buffer pool guards, etc.

### Zerocopy Safety

All struct field access uses the `zerocopy` crate for alignment-safe serialization:
- `read_from_prefix` / `write_to_prefix` — copy-based, no alignment needed
- `ref_from_prefix` / `mut_from_prefix` — zero-copy reference (requires alignment)
- All structs derive `FromBytes`, `IntoBytes`, `KnownLayout`, `Immutable`

### Layout Assertions

Each struct uses compile-time assertions to verify binary layout:
```rust
const _: () = assert!(size_of::<BTreeInnerHeader>() == 8);
```

## Page Buffer Layout

Every page starts with a 16-byte `UberPageHeader`:

```
Offset | Size | Field         | Type
-------|------|---------------|------
0      | 8    | page_lsn      | u64 (Lsn)
8      | 4    | page_id        | u32 (LPageId)
12     | 1    | page_type_id   | u8
13     | 3    | _pad           | [u8; 3]
```

The `page_type_id` identifies which overlay should interpret the remaining bytes.

## Page Types

```
Value | PageType              | Overlay                  | File
------|----------------------|--------------------------|------
  0   | Unused               | UnusedPage               | any
  1   | HeapPage             | HeapPage                 | .heap
  2   | BTreeInner           | BTreeInnerPage           | .idx
  3   | BTreeLeaf            | BTreeLeafPage            | .idx
 32   | HeapFileHeader       | HeapFileHeaderPage        | .heap
 33   | IndexFileHeader      | IndexFileHeaderPage       | .idx
 34   | DirectoryFileHeader  | DirectoryFileHeaderPage   | .dir
 64   | DirectoryInner       | DirectoryInnerPage        | .dir
 65   | DirectoryLeaf        | DirectoryLeafPage         | .dir
```

## Module Structure

```
src/page/overlays/
├── mod.rs                     # Re-exports
├── common/
│   ├── mod.rs
│   └── error.rs               # ConvertError (zerocopy wrapper)
├── unknown.rs                 # UnknownPage — untyped page access
├── unused.rs                  # UnusedPage — free page (type 0)
├── directory/
│   ├── mod.rs                 # Re-exports
│   ├── error.rs               # OverlayError enum
│   ├── inner.rs               # DirectoryInnerPage (B-Tree routing)
│   └── leaf.rs                # DirectoryLeafPage (B-Tree terminal)
├── file_header/
│   ├── mod.rs                 # Re-exports
│   ├── directory.rs           # DirectoryFileHeaderPage (global metadata)
│   ├── heap.rs                # HeapFileHeaderPage (table metadata)
│   └── index.rs               # IndexFileHeaderPage (index metadata)
├── table/
│   ├── mod.rs
│   └── heap_page.rs           # HeapPage (slotted tuple storage)
└── index/
    ├── mod.rs                 # Re-exports
    ├── error.rs               # OverlayError enum
    ├── btree_inner.rs         # BTreeInnerPage (index routing)
    └── btree_leaf.rs          # BTreeLeafPage (index terminal)
```

## Overlay Categories

### File Headers

Each database file has a header page at offset 0 that stores per-file metadata. All file headers share a consistent API:

| Method | Description |
|--------|-------------|
| `new(data)` | Wrap buffer (panics if not 4096 bytes) |
| `from_buffer(data)` | Wrap + validate PageType |
| `init(data, ...)` | Initialize fresh page with correct type and magic |
| `uber_header()` / `uber_header_mut()` | Access UberPageHeader |
| `data()` / `data_mut()` | Access type-specific Data struct |
| `as_buffer()` / `as_buffer_mut()` | Raw buffer access |

#### DirectoryFileHeaderPage (page 0 of `.dir`)

Global database metadata — the entry point for the entire storage system.

```
Bytes 16+ | Field               | Type
----------|---------------------|------
0-7       | magic               | [u8; 8] ("ABDB_DIR")
8-15      | next_tx_id          | u64 (TxnId)
16-23     | last_checkpoint_lsn | u64 (Lsn)
24-27     | next_page_id        | u32 (LPageId)
28-31     | dir_root_page       | u32 (DirPageId)
32-35     | next_file_id        | u32 (FileId)
36-39     | _pad                | [u8; 4]
```

#### HeapFileHeaderPage (page 0 of `.heap`)

Per-table file metadata.

```
Bytes 16+ | Field          | Type
----------|----------------|------
0-7       | magic          | [u8; 8] ("ABDB_HEP")
8-11      | num_pages      | u32
12-15     | table_oid      | u32 (OId)
16-19     | free_list_root | u32
20-21     | version        | u16
22-23     | _pad           | u16
```

#### IndexFileHeaderPage (page 0 of `.idx`)

Per-index file metadata.

```
Bytes 16+ | Field      | Type
----------|------------|------
0-7       | magic      | [u8; 8] ("ABDB_IDX")
8-11      | num_pages  | u32
12-15     | index_oid  | u32 (OId)
16-19     | root_page  | u32 (LPageId)
20-23     | table_oid  | u32 (OId)
24-25     | version    | u16
26-27     | _pad       | u16
```

### B-Tree Pages (Directory and Index)

Both the page directory (`.dir`) and indexes (`.idx`) use B-Tree structures. The directory and index B-Trees share identical page layouts but use different `PageType` values and serve different purposes:

| | Directory B-Tree | Index B-Tree |
|--|------------------|--------------|
| Purpose | Map logical PageId → physical location | Map index key → heap RecordId |
| Inner PageType | `DirectoryInner` (64) | `BTreeInner` (2) |
| Leaf PageType | `DirectoryLeaf` (65) | `BTreeLeaf` (3) |
| Key type | `u64` (logical page ID) | `u64` (index key) |
| Value type | `(LPageId, SlotId)` physical location | `(LPageId, SlotId)` heap RecordId |

#### Inner Page Layout (4096 bytes)

Routing nodes that guide B-Tree searches to the correct child.

```
Offset  | Size | Field
--------|------|----------------
0-15    | 16   | UberPageHeader
16-17   | 2    | num_keys (u16)
18-19   | 2    | flags (u16)
20-23   | 4    | leftmost_child (LPageId)
24-4095 | 4072 | Entry array (up to 254 entries × 16 bytes)
```

Each inner entry is 16 bytes:
```
Bytes 0-7:   separator_key (u64)
Bytes 8-11:  right_child   (LPageId)
Bytes 12-15: _padding      (u32)
```

**Routing semantics**: An inner node with `n` separator keys has `n + 1` children. For a search key `k`, the node routes to `leftmost_child` if `k < sep[0]`, otherwise to `entry[i].right_child` where `sep[i]` is the largest separator ≤ `k`.

**Capacity**: MAX_KEYS = 254, safe insert threshold = 253, merge threshold = 127.

#### Leaf Page Layout (4096 bytes)

Terminal nodes that store the actual key-to-value mappings.

```
Offset  | Size | Field
--------|------|----------------
0-15    | 16   | UberPageHeader
16-17   | 2    | num_entries (u16)
18-19   | 2    | flags (u16)
20-23   | 4    | next_page (LPageId) — right sibling
24-27   | 4    | prev_page (LPageId) — left sibling
28-31   | 4    | reserved_lo (u32) — future MVCC
32-35   | 4    | reserved_hi (u32) — future MVCC
36-39   | 4    | _padding (u32)
40-4095 | 4056 | Entry array (up to 253 entries × 16 bytes)
```

Each leaf entry is 16 bytes:
```
Bytes 0-7:   key         (u64)
Bytes 8-11:  record_page (LPageId)
Bytes 12-13: record_slot (SlotId)
Bytes 14-15: _padding    (u16)
```

**Capacity**: MAX_ENTRIES = 253, safe insert threshold = 252, merge threshold = 126.

**Sibling pointers**: `next_page` / `prev_page` form a doubly-linked list for range scans.

#### B-Tree Page API

Both inner and leaf overlays share this API pattern:

| Method | Inner | Leaf | Description |
|--------|:-----:|:----:|-------------|
| `new(data)` | x | x | Wrap buffer |
| `from_buffer(data)` | x | x | Wrap + validate type |
| `init(data, ...)` | x | x | Initialize fresh page |
| `entry(index)` | x | x | Read entry by position |
| `find_child(key)` | x | | Route to child page |
| `find_separator_slot(key)` | x | | Binary search for insert position |
| `find_slot(key)` | | x | Binary search for key |
| `lookup(key)` | | x | Find value for key |
| `insert_separator(key, child)` | x | | Insert separator maintaining order |
| `insert_entry(key, page, slot)` | | x | Insert entry maintaining order |
| `delete_separator(index)` | x | | Remove separator by index |
| `delete_entry(index)` | | x | Remove entry by index |
| `delete_by_key(key)` | | x | Remove entry by key |
| `update_child(index, child)` | x | | Update child pointer |
| `update_entry(key, page, slot)` | | x | Update value for key |
| `is_safe_for_insert()` | x | x | Latch crabbing check |
| `needs_split()` | x | x | At capacity? |
| `can_merge()` | x | x | Below merge threshold? |

### Heap Pages (Table Data)

`HeapPage` stores tuples in a slotted page format:

```
Offset  | Size | Field
--------|------|----------------
0-15    | 16   | UberPageHeader
16-17   | 2    | num_slots (u16)
18-19   | 2    | data_offset (u16) — start of free space
20-23   | 4    | _pad
24+     | var  | Slot array (4 bytes each: offset u16, length u16)
...     | var  | Free space
...     | var  | Tuple data (grows downward from end of page)
```

Tuples are stored from the end of the page growing downward. The slot array grows from byte 24 downward. When they meet, the page is full.

## Data Flow

```
Storage (Disk)
  ↓ read 4096-byte buffer
BufferPool (pin in memory, acquire latch)
  ↓ pass buffer reference to overlay
Overlay (typed access to page structure)
  ↓ return structured data
Accessor (business logic — scan, insert, delete)
  ↓ return tuples, RecordIds
Catalog (schema metadata)
```

## Thread Safety

All overlay methods are synchronous and pure — no I/O, no blocking, no async.
Thread safety is enforced at the BufferPool level:
- **Read methods** can be called under shared latch
- **Write methods** require exclusive latch (caller's responsibility)

Overlays themselves do not hold locks or latches. They are short-lived wrappers that exist only while the caller holds the appropriate latch on the buffer.

## Latch Crabbing

The B-Tree overlays support latch crabbing (also called "crab-stepping") through safety threshold checks:

- `is_safe_for_insert()` — returns `true` if the page has room for an insert without splitting. When traversing the B-Tree for an insert, if a child is safe, the parent latch can be released.
- `needs_split()` — returns `true` if the page is at capacity.
- `can_merge()` — returns `true` if the page is below the merge threshold and should be considered for merging with a sibling.

## Future Work

- **MVCC**: `reserved_lo` / `reserved_hi` fields in leaf headers are reserved for transaction visibility. The `TransactionVisibility` error variant exists but is not yet used.
- **Heap compaction**: `HeapPage::delete()` marks slots as deleted but does not reclaim space. A `compact()` method will be needed.
- **Variable-width index keys**: Current B-Tree index pages use fixed `u64` keys. Supporting variable-width keys (e.g., composite or string keys) would require a slotted layout similar to HeapPage.
