# ABDB - Implementation Progress

> No WAL — all writes go directly to disk via the buffer pool.
> Last updated: 2026-04-04

---

## Architecture Overview

```
SQL String
   |
   v
Parser (sqlparser crate)
   |
   v
Binder (semantic analysis)
   |
   v
Planner (logical plan)
   |
   v
Optimizer (physical plan)
   |
   v
Executor (plan interpretation)
   |
   v
Accessor (tuple-level ops)
   |--- heap.rs  (table scan/insert/get/delete)
   |--- btree.rs (index scan/insert/get/delete)
   |--- visibility.rs (MVCC)
   |--- catalog_cache.rs (metadata)
   |
   v
Buffer Pool (page-level I/O, latching, eviction)
   |
   v
Storage Layer
   |--- DiskManager (logical <-> physical page I/O)
   |--- PageAllocator (allocate/deallocate pages in files)
   |--- PageDirectory (logical PageId -> physical location B-Tree)
   |
   v
Disk (files: .heap, .idx, .dir)
```

---

## 1. Page Layer

### Page Overlays (zero-copy typed views over 4K buffers)

- [x] `UberPageHeader` — common header on every page (page_type, page_id)
- [x] `PageType` enum — all page type discriminators
- [x] `UnknownPage` — read page_type from raw buffer
- [x] `UnusedPage` — placeholder for uninitialized pages

### File Header Pages

- [x] `HeapFileHeaderPage` — per-table metadata (num_pages, etc.)
- [x] `IndexFileHeaderPage` — per-index metadata (num_pages, root_page)
- [x] `DirectoryFileHeaderPage` — global metadata for page directory
- [x] File header page unit tests (init, validation, mutation)

### Heap Pages

- [x] `HeapPage` — slotted page format (insert, delete, get, get_mut)
- [x] Slot directory management (grow from header end)
- [x] Free space tracking within pages
- [ ] Heap page compaction (defragment deleted slots in-place)
- [ ] Heap page unit tests

### Index B-Tree Pages

- [x] `BTreeInnerPage` — routing/separator pages with binary search
- [x] `BTreeLeafPage` — data leaf pages with sibling chains
- [x] Inner page: `find_child`, `insert_separator`, `is_safe_for_insert`
- [x] Leaf page: `insert_entry`, `delete_entry`, `find_entry`, sibling ptrs
- [x] Leaf page: merge support (`is_underfull`, steal/merge helpers)
- [x] Compile-time size assertions for page layout correctness
- [ ] Index page unit tests (inner and leaf)
- [ ] Variable-width index keys (currently fixed u64 keys only)

### Directory B-Tree Pages

- [x] `DirectoryInnerPage` — routing pages for page directory
- [x] `DirectoryLeafPage` — leaf pages for page directory
- [ ] Directory page unit tests

---

## 2. Storage Layer

### DiskManager

- [x] `DiskManager` trait definition (`read_page`, `write_page`, `read_page_at_loc`, `write_page_at_loc`, `new_page`)
- [ ] Concrete `DiskManager` implementation (file I/O with `tokio::fs`)
- [ ] File naming convention (FileId -> filesystem path)
- [ ] File creation and initialization (new heap/index/directory files)
- [ ] Error handling for I/O failures

### PageAllocator

- [x] `PageAllocator` trait definition (`allocate`, `deallocate`)
- [ ] Concrete `PageAllocator` implementation
- [ ] Free page tracking (free list or bitmap)
- [ ] File expansion (grow file when no free pages)
- [ ] File truncation (shrink file when trailing pages are free)

### PageDirectory

- [x] `PageDirectory` trait definition (`lookup`, `add_page`, `update_page`, `delete_page`, `flush_all_dirty`)
- [ ] Concrete `PageDirectory` implementation (B-Tree in .dir file)
- [ ] Directory B-Tree bootstrap (initial root creation)
- [ ] Logical-to-physical page mapping insert/lookup/delete
- [ ] PageDirectory error variants (currently empty enum)

### AlignedBuffer

- [x] 4K-aligned memory allocation for page I/O

---

## 3. Buffer Pool

### Core BufferPool

- [x] `BufferPool` trait definition (all method signatures)
- [x] Concrete `BufferPool<D>` struct with frame management
- [x] Frame metadata tracking (Vacant/Loaded, dirty bit, page IDs)
- [x] Pin count management (atomic increment/decrement)
- [x] LPageId <-> frame index mapping (HashMap)
- [x] PPageId <-> frame index mapping (HashMap)
- [x] Vacant frame set tracking

### Page Fetching

- [x] `fetch_page_write` — load page + exclusive latch
- [x] `fetch_page_read` — load page + shared latch
- [x] `fetch_page_at_loc_write` — load by physical location + exclusive latch
- [ ] `fetch_page_at_loc_read` — load by physical location + shared latch (stubbed `todo!()`)
- [ ] `new_page` — allocate new page through buffer pool (stubbed `todo!()`)
- [x] `load_page_as_unevictable` — pin page permanently (for catalog)
- [ ] `load_page_loc_as_unevictable` — pin by physical location (stubbed `todo!()`)

### Dirty Page Management

- [ ] `mark_dirty` on `PageWriteGuard` (stubbed `todo!()`)
- [ ] `flush_all_dirty` — write all dirty pages to disk (stubbed `todo!()`)
- [ ] Dirty bit propagation on write guard drop

### Eviction

- [x] `EvictionPolicy` trait definition (`find_victim`, `record_access`, `set_evictable`)
- [x] Eviction loop in buffer pool (victim selection, dirty writeback, metadata cleanup)
- [x] Eviction policy integration (record_access on guard creation)
- [ ] Concrete eviction policy (LRU-K, Clock, or LRU implementation)
- [ ] Evictor error variants (currently empty enum)

### RAII Guards

- [x] `PinGuard` — auto-decrement pin on drop
- [x] `PageReadGuard` — shared latch + deref to `PageBuffer`
- [x] `PageWriteGuard` — exclusive latch + deref_mut to `PageBuffer`
- [x] `downgrade` — convert write guard to read guard (latch downgrade)
- [ ] `commit_wal` — WAL flush on guard (can be removed since no WAL)

---

## 4. Accessor Layer

### Heap Operations (table data)

- [x] `scan` — sequential scan over all visible tuples (async stream)
- [x] `insert` — insert tuple with MVCC header, find free space or extend file
- [x] `get` — fetch single tuple by RecordId with visibility check
- [x] `delete` — soft-delete by setting XMAX
- [x] `update` — MVCC update (stamp XMAX on old version, insert new version)

### B-Tree Index Operations

- [x] `scan` — range scan via leaf sibling chain (async stream)
- [x] `insert` — insert with latch crabbing and cascading splits
- [x] `get` — point lookup (unique index)
- [x] `delete` — delete entry from leaf
- [x] `find_leaf` — read-only traversal root to leaf
- [x] `find_leaf_with_path` — traversal recording ancestor path
- [x] `find_leaf_for_write` — latch-crabbing write traversal
- [x] `split_leaf` — leaf split with sibling pointer updates
- [x] `split_inner` — inner page split with separator push-up
- [x] `insert_into_ancestors` — cascading split propagation
- [x] Leaf merge on delete (`try_merge_leaf`, `find_merge_candidate`)
- [ ] Inner page merge cascading (currently only leaf merges are performed)
- [ ] B-Tree bulk loading

### MVCC Visibility

- [x] `is_visible` — check tuple visibility against transaction
- [x] `read_xmin` / `read_xmax` — extract header fields
- [x] `write_xmax` — stamp deletion transaction
- [x] `make_header` — create MVCC tuple header
- [x] Unit tests for visibility rules (6 tests)

### Catalog Cache

- [x] `CatalogCache` — in-memory HashMap-based metadata store
- [x] Lookup by OID and by name (tables, indexes, columns)
- [x] Registration methods (register_table, register_index, register_columns)
- [x] Deregistration methods (deregister_table, deregister_index)
- [x] Query methods (has_table_name, has_index_name, indexes_for_table)
- [x] System table bootstrapping (`sys_tables`, `sys_columns`, `sys_indexes`)

### Accessor Trait & Impl

- [x] `Accessor` trait with all method signatures (table, index, catalog, DDL)
- [x] `AccessorImpl<B>` concrete implementation
- [x] Table ops delegation (scan, insert, get, delete, update)
- [x] Index ops delegation (scan, insert, get, delete)
- [x] Catalog ops (synchronous, from cache)
- [x] OID and FileId allocation (AtomicU32 monotonic counters)
- [x] DDL ops — `create_table` (init heap file header, register in catalog cache)
- [x] DDL ops — `drop_table` (deregister from catalog, check for active indexes)
- [x] DDL ops — `create_index` (init index file header, register in catalog cache)
- [x] DDL ops — `drop_index` (deregister from catalog cache)
- [ ] DDL: system table persistence (insert into sys_tables/sys_columns/sys_indexes — deferred to DB bootstrap)
- [ ] DDL: physical page deallocation on drop (requires storage layer file-level API)

---

## 5. Data Types & Tuple Layout

### DataBox

- [x] `DataType` enum (Bool, I8-I64, U8-U64, F32, F64, String)
- [x] `Value` enum (runtime typed values)
- [x] `TryFrom<u8>` for DataType
- [x] `Into<u8>` for DataType
- [ ] `Value` comparison operators (Ord, PartialOrd, Eq)
- [ ] `Value` arithmetic operators
- [ ] `Value` serialization to/from bytes
- [ ] `Value` type coercion/casting

### TupleLayout

- [x] Layout calculation from column definitions
- [x] Null bitmap offset computation
- [x] Fixed-width column offset computation with alignment
- [x] Variable-length string pointer layout (u16 length + u16 offset)
- [ ] `read_field` — read a typed field from a tuple byte slice
- [ ] `write_field` — write a typed value into a tuple byte slice
- [ ] `serialize_tuple` — convert Vec<Value> to byte slice using layout
- [ ] `deserialize_tuple` — convert byte slice to Vec<Value> using layout

---

## 6. Binder (SQL Semantic Analysis)

### Infrastructure

- [x] `BindError` enum with error variants
- [x] `Binder<A>` struct with table scope stack
- [x] `BoundStatement` enum (CreateTable, Insert, Update, Delete, Select)
- [x] `BoundExpr` enum (Constant, ColumnRef, UnaryOp, BinaryOp, Star)
- [x] `BoundTableRef` enum (BaseTable, Join, CrossProduct, Subquery)
- [x] `BoundSelect` struct with all clauses
- [x] `BoundCreateTable`, `BoundInsert`, `BoundUpdate`, `BoundDelete` structs
- [x] `ColumnDef` struct
- [x] Operator enums (UnaryOperator, BinaryOperator, JoinType)

### Binding Methods (all stubbed `todo!()`)

- [ ] `bind_statement` — dispatch to specific binders
- [ ] `bind_create_table` — validate table definition
- [ ] `bind_insert` — resolve table, validate columns and values
- [ ] `bind_update` — resolve table, validate assignments
- [ ] `bind_delete` — resolve table, validate WHERE clause
- [ ] `bind_query` — bind top-level query (with ORDER BY, LIMIT)
- [ ] `bind_select` — bind SELECT with FROM, WHERE, GROUP BY
- [ ] `bind_table_with_joins` — resolve table references with joins
- [ ] `bind_table_ref` — resolve single table reference
- [ ] `bind_join_constraint` — resolve ON/USING clause
- [ ] `bind_select_list` — resolve projection columns
- [ ] `bind_expr` — recursive expression binding
- [ ] `bind_value` — literal value binding
- [ ] `bind_data_type` — SQL type to DataType mapping
- [ ] `bind_binary_op` — SQL binary op to BinaryOperator
- [ ] `bind_unary_op` — SQL unary op to UnaryOperator
- [ ] `push_table_scope` / `pop_table_scope` — scope management
- [ ] `resolve_column` — column name resolution in scope

---

## 7. Planner (Logical Plan Generation)

### Infrastructure

- [x] `PlanError` enum
- [x] `Planner<A>` struct
- [x] `PlanNode` enum with all operator variants
- [x] `Schema` struct for plan node output schemas
- [x] DDL nodes: `CreateTableNode`, `DropTableNode`
- [x] DML nodes: `InsertNode`, `UpdateNode`, `DeleteNode`
- [x] Scan nodes: `SeqScanNode`, `IndexScanNode`
- [x] Relational nodes: `FilterNode`, `ProjectionNode`
- [x] Join nodes: `NestedLoopJoinNode`, `HashJoinNode`, `MergeJoinNode`
- [x] Utility nodes: `SortNode`, `LimitNode`, `ValuesNode`

### Planning Methods (all stubbed `todo!()`)

- [ ] `plan` — dispatch bound statement to plan builder
- [ ] `plan_create_table` — produce CreateTableNode
- [ ] `plan_insert` — produce InsertNode with child ValuesNode
- [ ] `plan_update` — produce UpdateNode with child scan
- [ ] `plan_delete` — produce DeleteNode with child scan
- [ ] `plan_select` — produce scan + filter + projection tree
- [ ] `plan_table_ref` — produce scan or join subtree
- [ ] `plan_base_table` — produce SeqScanNode
- [ ] `plan_join` — produce join node from bound join

---

## 8. Optimizer (Query Optimization)

### Infrastructure

- [x] `Optimizer<A>` struct
- [x] `optimize` method pipeline (6 passes)

### Optimization Passes (all stubbed `todo!()`)

- [ ] `push_down_filters` — push predicates closer to scans
- [ ] `push_down_projections` — eliminate unnecessary columns early
- [ ] `reorder_joins` — reorder join order for cost reduction
- [ ] `choose_join_algorithm` — select NLJ/Hash/Merge per join
- [ ] `choose_access_method` — pick SeqScan vs IndexScan
- [ ] `merge_operators` — fuse adjacent filter/projection nodes

---

## 9. Executor (Volcano / Iterator Model)

> Not yet started — no executor module exists.
> Uses the **volcano model**: each operator implements a `next()` method that
> pulls one tuple at a time from its child operator. Async streams (`Stream`)
> from the accessor layer map naturally to this pull-based iterator interface.

- [ ] `Executor` trait — async `next() -> Option<Tuple>` pull interface
- [ ] `SeqScan` executor — call `accessor.table_scan`, yield tuples
- [ ] `IndexScan` executor — call `accessor.index_scan`, yield tuples
- [ ] `Filter` executor — pull from child, evaluate predicate, yield matching
- [ ] `Projection` executor — pull from child, evaluate expressions, yield projected
- [ ] `NestedLoopJoin` executor — nested iteration over two children
- [ ] `HashJoin` executor — build hash table from left, probe with right
- [ ] `Sort` executor — materialize child, sort (external sort for large sets)
- [ ] `Limit` executor — yield at most N tuples from child
- [ ] `Insert` executor — pull from child (Values), call `accessor.table_insert`
- [ ] `Update` executor — pull from child, call `accessor.table_update`
- [ ] `Delete` executor — pull from child, call `accessor.table_delete`
- [ ] `CreateTable` executor — call `accessor.create_table`
- [ ] `DropTable` executor — call `accessor.drop_table`
- [ ] `CreateIndex` executor — call `accessor.create_index`, backfill via scan + index_insert
- [ ] `DropIndex` executor — call `accessor.drop_index`
- [ ] `Values` executor — produce literal rows from bound expressions
- [ ] Expression evaluator (evaluate `BoundExpr` against a tuple row)

---

## 10. Transaction Manager

> Not yet started — only the `Txn` struct exists.

- [x] `Txn` struct (id, isolation level)
- [x] `IsolationLevel` enum (ReadUncommitted, ReadCommitted, Snapshot)
- [ ] Transaction manager (begin, commit, abort)
- [ ] Transaction ID generation (monotonic counter)
- [ ] Active transaction tracking (for visibility checks)
- [ ] Commit/abort status tracking
- [ ] Snapshot isolation — maintain read snapshot of active txns at start

---

## 11. Database Initialization & Bootstrapping

- [ ] Database creation (create .dir, system .heap files)
- [ ] System table initialization (sys_tables, sys_columns, sys_indexes)
- [ ] Catalog loading on startup (scan system tables into CatalogCache)
- [ ] File ID allocation (monotonic counter persisted in directory header)
- [ ] OID allocation (monotonic counter for tables/columns/indexes)

---

## 12. SQL Frontend (End-to-End Pipeline)

- [x] `sqlparser` dependency for parsing
- [ ] Parse SQL string -> AST (wire up sqlparser)
- [ ] AST -> BoundStatement (binder)
- [ ] BoundStatement -> PlanNode (planner)
- [ ] PlanNode -> optimized PlanNode (optimizer)
- [ ] PlanNode -> query result (executor)
- [ ] Result formatting (rows -> display)
- [ ] Error reporting (user-facing error messages)

---

## 13. Integration & Testing

- [x] Visibility unit tests (6 tests)
- [x] File header page unit tests (9 tests across 3 page types)
- [ ] Heap page unit tests (insert, delete, get, free space, slot reuse)
- [ ] B-Tree page unit tests (inner: find_child, split; leaf: insert, delete, scan)
- [ ] Buffer pool unit tests (fetch, eviction, dirty writeback, pin/unpin)
- [ ] DiskManager integration tests (read/write round-trip)
- [ ] Accessor integration tests (table CRUD, index CRUD)
- [ ] B-Tree integration tests (insert many, scan range, delete, split cascades)
- [ ] Binder tests (each SQL statement type)
- [ ] Planner tests (plan shape for each statement)
- [ ] Optimizer tests (verify transform correctness)
- [ ] Executor tests (each operator)
- [ ] End-to-end SQL tests (CREATE TABLE, INSERT, SELECT, UPDATE, DELETE)
- [ ] Concurrent access tests (multiple transactions, latch correctness)

---

## 14. `main.rs` & CLI

- [x] Placeholder main (`println!("Hello, world!")`)
- [ ] Database instance initialization
- [ ] REPL / interactive SQL shell
- [ ] Command-line argument parsing (db path, buffer pool size, etc.)

---

## Summary

| Layer | Status | Completion |
|-------|--------|------------|
| Page Overlays | Done | ~90% |
| Storage Layer | Traits only | ~10% |
| Buffer Pool | Mostly done | ~65% |
| **Accessor** | **Complete** | **~95%** |
| Data Types / Layout | Types done, serialization missing | ~40% |
| Binder | Scaffolded, all stubs | ~15% |
| Planner | Scaffolded, all stubs | ~15% |
| Optimizer | Scaffolded, all stubs | ~10% |
| Executor (Volcano) | Not started | 0% |
| Transaction Manager | Struct only | ~10% |
| DB Bootstrap | Not started | 0% |
| SQL Frontend | Parser only | ~5% |
| Testing | Minimal | ~10% |
| CLI / main | Placeholder | ~5% |
