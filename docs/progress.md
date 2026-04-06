# ABDB - Implementation Progress

> No WAL — all writes go directly to disk via the buffer pool.
> Last updated: 2026-04-06

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
- [x] `Value` PartialOrd — SQL-style null ordering, cross-type f64 promotion
- [x] `Value` PartialEq
- [x] `Value::to_bytes` — serialize to little-endian bytes
- [x] `Value::from_bytes` — deserialize from bytes + DataType
- [x] `Value::cast` — widening numeric casts, any -> String, int -> Bool
- [x] `Value::to_i64`, `to_u64`, `to_f64` — lossless numeric conversion
- [x] `Value::data_type`, `is_null` helpers
- [x] `DataType` helpers — `fixed_size`, `is_numeric`, `is_signed_int`, `is_unsigned_int`, `is_float`
- [x] Unit tests — roundtrip, cast, ordering, type helpers

### TupleLayout

- [x] Layout calculation from column definitions
- [x] Null bitmap offset computation
- [x] Fixed-width column offset computation with alignment
- [x] Variable-length string pointer layout (u16 length + u16 offset)
- [x] `read_field` — read a typed field from a tuple byte slice (all types incl. String)
- [x] `write_field` — write a typed value into a tuple byte slice (fixed-width + null bitmap)
- [x] Unit tests — layout offsets, read/write roundtrip, string returns false

---

## 6. Binder (SQL Semantic Analysis)

### Infrastructure

- [x] `BindError` enum with error variants (25+ variants)
- [x] `Binder<A>` struct with accessor + scope + OID allocator
- [x] `BoundStatement` enum (all DDL, DML, Explain)
- [x] `BoundExpr` / `BoundExprKind` — full expression tree (Literal, ColumnRef, BinaryOp, UnaryOp, IsNull, Like, Between, In, InSubquery, Exists, Subquery, Function, Cast, Case)
- [x] `BoundTableRef` enum (BaseTable, Subquery)
- [x] `BoundSelect` struct with all clauses (projections, from, joins, where, group_by, having, order_by, limit, offset, distinct)
- [x] `BoundCreateTable`, `BoundDropTable`, `BoundAlterTable`, `BoundCreateIndex`, `BoundDropIndex`
- [x] `BoundInsert`, `BoundUpdate`, `BoundDelete`
- [x] `BoundJoinKind`, `BoundJoinCondition` (On, Using, Natural, None)
- [x] `FunctionKind` enum (Count, Sum, Avg, Min, Max, Coalesce, Nullif, Upper, Lower, Length, Abs)
- [x] `OutputColumn` struct
- [x] `OidAllocator` — monotonic OID generation for binder
- [x] `Scope` / `ScopeColumn` — column resolution with qualifier support

### Binding Methods

- [x] `bind_statement` — dispatch to specific binders
- [x] `bind_create_table` — validate columns, PK, FK, allocate OIDs
- [x] `bind_drop_table` — resolve table, collect index OIDs
- [x] `bind_alter_table` — add/drop/rename column, alter type, FK, PK
- [x] `bind_create_index` / `bind_drop_index`
- [x] `bind_insert` — resolve table, validate column count, bind source (Values/Select)
- [x] `bind_update` — resolve table, bind assignments + WHERE
- [x] `bind_delete` — resolve table, bind WHERE
- [x] `bind_select` — full SELECT binding (FROM, joins, WHERE, GROUP BY, HAVING, projections, ORDER BY, LIMIT, DISTINCT)
- [x] `bind_table_ref` — BaseTable (with alias) and Subquery
- [x] `bind_join_condition` — ON, USING, NATURAL, cross join
- [x] `bind_expr` — recursive expression binding for all BoundExprKind variants
- [x] `bind_function` — resolve function name to FunctionKind, validate args
- [x] Scope management — push/pop for subqueries, column resolution with ambiguity detection
- [ ] Known issue: `bind_join_condition` moves `join.kind` causing borrow-after-move (pre-existing bug in binder.rs:469)

---

## 7. Planner (Logical Plan Generation)

### Infrastructure

- [x] `Planner` struct (stateless, all methods are associated functions)
- [x] `LogicalPlan` enum — SeqScan, IndexScan, Filter, Projection, Join, Aggregate, Sort, Limit, Distinct, Insert, Update, Delete, Values, DDL pass-through, Nothing
- [x] `Schema` struct for plan node output schemas (with `len()`, `empty()`)
- [x] `SeqScan`, `IndexScan` — scan nodes with table/column metadata
- [x] `Filter`, `Projection`, `Sort`, `Limit`, `Distinct` — unary relational nodes
- [x] `Join` — binary join with kind + condition
- [x] `Aggregate` / `AggregateExpr` — group-by + aggregate functions
- [x] `LogicalInsert`, `LogicalUpdate`, `LogicalDelete` — DML nodes
- [x] `Values` — inline literal rows
- [x] `SortKey` — expression + asc/desc + nulls_first

### Planning Methods

- [x] `plan` — dispatch BoundStatement to specific planner
- [x] `plan_select` — full pipeline: FROM -> WHERE -> GROUP BY/HAVING -> Projection -> DISTINCT -> ORDER BY -> LIMIT
- [x] `plan_from` — resolve FROM clause with joins, merge schemas
- [x] `plan_table_ref` — BaseTable -> SeqScan, Subquery -> recursive plan_select
- [x] `plan_insert` — Values or Select source, map target columns, rows_affected schema
- [x] `plan_update` — SeqScan + optional Filter, assignment propagation
- [x] `plan_delete` — SeqScan + optional Filter
- [x] `collect_aggregates` — extract aggregate expressions from projections + having
- [x] `build_aggregate_schema` — construct output schema for aggregate nodes
- [x] DDL/transaction pass-through (CreateTable, DropTable, AlterTable, CreateIndex, DropIndex, Begin/Commit/Rollback)

---

## 8. Optimizer (Query Optimization)

### Infrastructure

- [x] `Optimizer<A>` struct with accessor + transaction
- [x] `PhysicalPlan` enum — all 23 operator variants (see Executor section)
- [x] Physical operator structs: `PhysSeqScan`, `PhysIndexScan`, `PhysFilter`, `PhysProjection`, `PhysNestedLoopJoin`, `PhysHashJoin`, `PhysHashAggregate`, `PhysStreamAggregate`, `PhysSort`, `PhysTopN`, `PhysLimit`, `PhysDistinct`, `PhysHashDistinct`, `PhysInsert`, `PhysUpdate`, `PhysDelete`, `PhysValues`
- [x] `PhysAggregateExpr`, `PhysSortKey` — aggregate and sort metadata

### Optimization Passes

- [x] `push_predicates` — recursive predicate pushdown through Filter, Projection, Join, Aggregate, Sort, Limit, Distinct, Update, Delete
- [x] Join predicate splitting — classify predicates as left-only, right-only, or cross-join; push single-side predicates into children
- [x] `to_physical` — logical-to-physical plan translation (SeqScan, IndexScan, Filter, Projection, Join, Aggregate, Sort, Limit, Distinct, DML, DDL)
- [x] `fuse_sort_limit` — merge adjacent Sort + Limit into TopN (recursive over entire tree)
- [x] Index scan selection — `try_index_scan` extracts index-compatible predicates (Eq, Lt, LtEq, Gt, GtEq on indexed columns), splits into range keys + residual
- [x] Hash join selection — `extract_equi_keys` detects equality conditions in ON/USING/NATURAL joins, extracts left/right keys + residual
- [x] Scope index manipulation — `shift_scope_indices` for join predicate rewriting, `collect_scope_indices` for side classification
- [x] Helper: `split_conjunctions` / `conjoin` — AND decomposition/recomposition
- [ ] `find_index_for_column` — currently returns None (index selection stub)
- [ ] Cost-based join reordering
- [ ] Projection pushdown (eliminate unused columns early)

---

## 9. Executor (In-Memory Materialized)

> **Complete** — all 23 PhysicalPlan variants handled. Memory-only, no disk spilling.
> See [docs/executor.md](executor.md) for full design documentation.

### Core

- [x] `Executor<A: Accessor>` struct — generic over accessor, takes Arc<A> + Txn
- [x] `execute(plan) -> ExecResult<Vec<Row>>` — async entry point
- [x] Recursive `exec()` dispatch — exhaustive match over all PhysicalPlan variants
- [x] `ExecError` enum (Accessor, Eval, Internal) with Display + Error impls
- [x] `Row` type alias (`Vec<Value>`)

### Expression Evaluator (`eval.rs`)

- [x] `eval_expr(expr, row) -> Value` — pure expression evaluation
- [x] Literal, ColumnRef, BinaryOp (arithmetic, comparison, logical, concat)
- [x] UnaryOp (Neg, Not), IsNull, IsNotNull
- [x] LIKE pattern matching (%, _) — iterative backtracking algorithm
- [x] BETWEEN (with negation), IN (with negation)
- [x] CASE (simple + searched), CAST (delegates to Value::cast)
- [x] Scalar functions: COALESCE, NULLIF, UPPER, LOWER, LENGTH, ABS
- [x] SQL three-valued NULL propagation (AND/OR short-circuit)
- [x] `eval_to_bool` helper — NULL/non-bool -> false

### Tuple Codec (`codec.rs`)

- [x] `decode_row(layout, columns, raw)` — raw tuple bytes -> Vec<Value> via TupleLayout
- [x] `encode_row(layout, columns, values)` — Vec<Value> -> raw tuple bytes (fixed + variable-length strings)

### Scan Operators

- [x] `SeqScan` — stream table via accessor, decode via TupleLayout, apply pushed predicates
- [x] `IndexScan` — range-scan B-tree, fetch tuples by RID via table_get, apply residual predicates

### Relational Operators

- [x] `Filter` — eval predicate, retain matching rows
- [x] `Projection` — eval expressions per row, with aggregate-alias resolution for projections above aggregate nodes
- [x] `NestedLoopJoin` — O(N*M) with Inner, Left/Right/Full Outer support via matched-row tracking
- [x] `HashJoin` — build on right, probe with left, residual predicate, all four join kinds
- [x] `HashAggregate` — group-by hashing, Accumulator per group (COUNT, SUM, AVG, MIN, MAX), DISTINCT aggregates via HashSet, scalar aggregates on empty input
- [x] `StreamAggregate` — delegates to HashAggregate (equivalent when fully materialized)
- [x] `Sort` — in-memory sort_by with null-aware comparator (ASC/DESC, NULLS FIRST/LAST)
- [x] `TopN` — full sort + skip(offset) + take(limit)
- [x] `Limit` — skip(offset) + take(limit)
- [x] `Distinct` / `HashDistinct` — HashSet-based dedup using serialized row keys
- [x] `Values` — evaluate literal expression rows

### DML Operators

- [x] `Insert` — execute source, map target to full table columns, encode via TupleLayout, call table_insert. Returns rows_affected
- [x] `Update` — scan_with_rids, evaluate assignments, delete old + insert new. Returns rows_affected
- [x] `Delete` — scan_with_rids, call table_delete. Returns rows_affected
- [x] `scan_with_rids` — specialized scan path preserving RecordIds through SeqScan/IndexScan/Filter chains

### DDL Operators

- [x] `CreateTable`, `DropTable`, `AlterTable`, `CreateIndex`, `DropIndex` — return empty rows (catalog mutation handled by session/accessor layer)
- [x] `Nothing` — return empty (transaction control: BEGIN/COMMIT/ROLLBACK)

### Hashing Utilities

- [x] `row_to_key` — deterministic byte serialization of Vec<Value> for HashMap keys (type-tagged, length-prefixed for strings)
- [x] `eval_key` — evaluate expressions then serialize for hash join/aggregate probing

### Known Limitations

- [ ] No spill-to-disk for sort, hash join, or aggregation
- [ ] Subquery expressions (IN SELECT, EXISTS, scalar subquery) return Null
- [ ] Compound aggregate expressions in projections (e.g. COUNT(*)+1) not rewritten
- [ ] TopN uses full sort instead of heap-based partial sort
- [ ] Update uses delete+insert instead of in-place update
- [ ] CREATE INDEX doesn't backfill existing table data into new index

---

## 10. Transaction Manager

- [x] `Txn` struct (id, isolation level)
- [x] `IsolationLevel` enum (ReadUncommitted, ReadCommitted, RepeatableRead, Snapshot, Serializable) with Display
- [x] `Transaction` struct (txn_id, read_ts, commit_ts, state, isolation_level)
- [x] `TxnState` enum (Active, Committed, Aborted)
- [x] `TransactionManager` — begin() with monotonic txn_id + read_ts
- [x] `current_ts()` — read current timestamp
- [ ] `commit()` — stubbed `todo!()`
- [ ] `abort()` — not yet implemented
- [ ] Active transaction tracking (for visibility checks)
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
- [x] Parser module — AST types (Statement, Expr, SelectStmt, etc.) with all SQL constructs
- [x] AST -> BoundStatement (binder — fully implemented)
- [x] BoundStatement -> LogicalPlan (planner — fully implemented)
- [x] LogicalPlan -> PhysicalPlan (optimizer — fully implemented)
- [x] PhysicalPlan -> Vec<Row> (executor — fully implemented)
- [x] `Session` struct — holds accessor, txn_manager, session state (Idle/InTransaction/Failed)
- [ ] `Session::execute_sql` — end-to-end pipeline wiring (stubbed `todo!()`)
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
| **Data Types / Layout** | **Complete** | **~95%** |
| **Binder** | **Complete** (1 known bug) | **~90%** |
| **Planner** | **Complete** | **~95%** |
| **Optimizer** | **Complete** (index selection stub) | **~85%** |
| **Executor** | **Complete** (memory-only, all 23 operators) | **~90%** |
| Transaction Manager | Begin works, commit/abort stubbed | ~30% |
| DB Bootstrap | Not started | 0% |
| SQL Frontend | All stages done, pipeline not wired | ~80% |
| Testing | Minimal | ~10% |
| CLI / main | Placeholder | ~5% |
