# Remaining Work for Full Database with Snapshot Isolation

> Last updated: 2026-04-07
> Scope: What's left to implement before having a fully working database with snapshot isolation and transaction support (ignoring WAL).

---

## Overview

The database has solid foundations in the storage layer, buffer pool, accessor layer, and now has a working query execution pipeline.

| Category | Status | Priority |
|----------|--------|----------|
| Transaction Manager (commit/rollback) | ✅ **IMPLEMENTED** | Done |
| Executor Layer | ✅ **IMPLEMENTED** | Done |
| Expression Evaluator | ✅ **IMPLEMENTED** | Done |
| Data Serialization | ✅ Already complete | Done |
| Binder/Planner/Optimizer | ✅ Already complete | Done |
| SQL Pipeline Integration | ✅ **IMPLEMENTED** | Done |
| Database Bootstrap | 0% complete | **HIGH** |
| DDL Persistence | Not started | **HIGH** |
| Update/Delete actual implementation | ✅ **IMPLEMENTED** | Done |
| Index Scan execution | ✅ **IMPLEMENTED** | Done |

---

## 1. Transaction Manager ✅ IMPLEMENTED

**File**: `src/transaction/mod.rs`

### Current State
- `begin()` - ✅ Works, allocates txn ID and captures read_ts, tracks active txns
- `commit()` - ✅ **IMPLEMENTED** - advances timestamp, marks committed, removes from active set
- `rollback()` - ✅ **IMPLEMENTED** - marks aborted, removes from active set
- `current_ts` - ✅ Now advances on each commit

### What's Implemented

#### 1.1 Commit Implementation
- Assigns commit timestamp (`commit_ts = current_ts.fetch_add(1)`)
- Marks transaction state as `Committed`
- Removes from active transaction set

#### 1.2 Rollback Implementation
- Marks transaction state as `Aborted`
- Removes from active transaction set
- Note: Undo of writes not yet implemented (would need write tracking)

#### 1.3 Active Transaction Tracking
- ✅ Maintains `HashSet<TxnId>` of currently active transactions
- ✅ `is_active(txn_id)` and `active_transaction_ids()` methods available

### Still TODO for Full Snapshot Isolation
- Write set tracking for proper rollback undo
- Visibility checks using commit timestamps (currently uses txn_id ordering)

### Snapshot Isolation Details

For true snapshot isolation, the visibility check needs enhancement:

```rust
// Current (simplified) visibility:
fn is_visible(tuple: &[u8], txn: &Txn) -> bool {
    let xmin = read_xmin(tuple);
    let xmax = read_xmax(tuple);
    xmin <= txn.id && (xmax == 0 || xmax > txn.id)
}

// What's needed for snapshot isolation:
fn is_visible(tuple: &[u8], txn: &Txn, txn_manager: &TxnManager) -> bool {
    let xmin = read_xmin(tuple);
    let xmax = read_xmax(tuple);

    // XMIN must be committed AND committed before our snapshot
    let xmin_committed = txn_manager.is_committed(xmin)
        && txn_manager.commit_ts(xmin) <= txn.read_ts;

    // XMAX must be 0 OR not committed OR committed after our snapshot
    let not_deleted = xmax == 0
        || !txn_manager.is_committed(xmax)
        || txn_manager.commit_ts(xmax) > txn.read_ts;

    xmin_committed && not_deleted
}
```

Required data structures:
```rust
pub struct TransactionManager {
    next_txn_id: AtomicU64,
    current_ts: AtomicU64,

    // NEW: Track transaction states and commit timestamps
    txn_states: RwLock<HashMap<TxnId, TxnState>>,
    commit_timestamps: RwLock<HashMap<TxnId, Timestamp>>,
    active_txns: RwLock<HashSet<TxnId>>,
}
```

---

## 2. Executor Layer ✅ IMPLEMENTED

**Location**: `src/executor/` module

The executor has been implemented with the following components:

### Implementation Approach

Uses a **materialized execution** model where each operator produces a complete `Vec<Tuple>` rather than streaming. This is simpler and works well for the current stage.

**Files**:
- `src/executor/mod.rs` - Module exports
- `src/executor/tuple.rs` - Tuple type for row data
- `src/executor/evaluate.rs` - Expression evaluator
- `src/executor/execute.rs` - Main execute function with all operators

### Implemented Executors

| Executor | Status | Notes |
|----------|--------|-------|
| `Values` | ✅ | Evaluates literal expressions |
| `SeqScan` | ✅ | Full table scan via accessor |
| `Filter` | ✅ | Predicate evaluation |
| `Projection` | ✅ | Expression evaluation |
| `NestedLoopJoin` | ✅ | All join types (Inner, Left, Right, Full, Cross) |
| `HashJoin` | ✅ | Inner and Left Outer |
| `HashAggregate` | ✅ | GROUP BY + aggregates (COUNT, SUM, AVG, MIN, MAX) |
| `Sort` | ✅ | ORDER BY with nulls handling |
| `Limit` | ✅ | LIMIT/OFFSET |
| `TopN` | ✅ | Fused Sort+Limit |
| `Distinct` | ✅ | DISTINCT |
| `Insert` | ✅ | INSERT via accessor |
| `Update` | ✅ | MVCC update (delete + insert) via accessor |
| `Delete` | ✅ | Delete via accessor.table_delete() |
| `IndexScan` | ✅ | Index scan via accessor + table fetch |
| DDL (Create/Drop/Alter) | ✅ | Returns success message |

---

## 3. Expression Evaluator ✅ IMPLEMENTED

**Location**: `src/executor/evaluate.rs`

### What's Implemented

- `evaluate_expr(expr, tuple)` - Evaluates any BoundExpr against a Tuple
- `evaluate_predicate(expr, tuple)` - Evaluates to boolean for WHERE clauses

### Supported Expression Types

| Expression | Status |
|------------|--------|
| Literals | ✅ |
| Column references | ✅ |
| Binary ops (=, <>, <, <=, >, >=, AND, OR, +, -, *, /, %, \|\|) | ✅ |
| Unary ops (NOT, -) | ✅ |
| IS NULL / IS NOT NULL | ✅ |
| CAST | ✅ |
| BETWEEN | ✅ |
| IN (list) | ✅ |
| LIKE | ✅ (basic % and _ patterns) |
| CASE/WHEN | ✅ |
| Scalar functions (COALESCE, NULLIF, UPPER, LOWER, LENGTH, ABS) | ✅ |
| Subqueries | ❌ Not supported |

### NULL Handling
- Proper SQL NULL semantics for comparisons
- AND/OR short-circuit with NULL
- Aggregates skip NULLs appropriately

---

## 4. Data Types & Tuple Layout (HIGH)

**Files**: `src/databox/databox.rs`, `src/tuple/layout.rs`

### Current State (~40%)
- `DataType` enum exists
- `Value` enum exists
- Layout calculation exists

### 4.1 Missing Value Operations

```rust
// src/databox/databox.rs

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (Value::I64(a), Value::I64(b)) => a.partial_cmp(b),
            (Value::F64(a), Value::F64(b)) => a.partial_cmp(b),
            (Value::String(a), Value::String(b)) => a.partial_cmp(b),
            (Value::Bool(a), Value::Bool(b)) => a.partial_cmp(b),
            // Handle type mismatches or cross-type comparisons
            _ => None,
        }
    }
}

impl Value {
    pub fn add(&self, other: &Value) -> Result<Value> { ... }
    pub fn sub(&self, other: &Value) -> Result<Value> { ... }
    pub fn mul(&self, other: &Value) -> Result<Value> { ... }
    pub fn div(&self, other: &Value) -> Result<Value> { ... }
    pub fn and(&self, other: &Value) -> Result<Value> { ... }
    pub fn or(&self, other: &Value) -> Result<Value> { ... }
}
```

### 4.2 Missing Tuple Serialization

```rust
// src/tuple/layout.rs

impl TupleLayout {
    /// Read a field value from a tuple byte slice
    pub fn read_field(&self, tuple: &[u8], field_idx: usize) -> Result<Value> {
        let col = &self.columns[field_idx];
        let offset = self.field_offsets[field_idx];

        match col.data_type {
            DataType::Bool => Ok(Value::Bool(tuple[offset] != 0)),
            DataType::I32 => {
                let bytes: [u8; 4] = tuple[offset..offset+4].try_into()?;
                Ok(Value::I32(i32::from_le_bytes(bytes)))
            }
            DataType::I64 => {
                let bytes: [u8; 8] = tuple[offset..offset+8].try_into()?;
                Ok(Value::I64(i64::from_le_bytes(bytes)))
            }
            DataType::String => {
                // Variable-length: read offset and length from pointer area
                let ptr_offset = self.var_len_ptr_offset(field_idx);
                let len = u16::from_le_bytes(tuple[ptr_offset..ptr_offset+2].try_into()?);
                let str_offset = u16::from_le_bytes(tuple[ptr_offset+2..ptr_offset+4].try_into()?);
                let str_bytes = &tuple[str_offset as usize..(str_offset + len) as usize];
                Ok(Value::String(String::from_utf8(str_bytes.to_vec())?))
            }
            // ... other types
        }
    }

    /// Write a field value into a tuple byte slice
    pub fn write_field(&self, tuple: &mut [u8], field_idx: usize, value: &Value) -> Result<()> {
        // Inverse of read_field
    }

    /// Serialize a vector of values into a tuple byte slice
    pub fn serialize(&self, values: &[Value]) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; self.tuple_size()];
        for (idx, value) in values.iter().enumerate() {
            self.write_field(&mut buf, idx, value)?;
        }
        Ok(buf)
    }

    /// Deserialize a tuple byte slice into a vector of values
    pub fn deserialize(&self, tuple: &[u8]) -> Result<Vec<Value>> {
        (0..self.columns.len())
            .map(|idx| self.read_field(tuple, idx))
            .collect()
    }
}
```

---

## 5. Binder, Planner, Optimizer (HIGH)

All three components are scaffolded with types but all methods are `todo!()`.

### 5.1 Binder (`src/binder/`)

**Status**: ~15% - All method bodies are `todo!()`

Key methods to implement:

| Method | Purpose |
|--------|---------|
| `bind_create_table` | Validate table doesn't exist, bind column defs |
| `bind_insert` | Resolve table, validate column count/types, bind values |
| `bind_select` | Resolve tables, bind columns, expressions, WHERE |
| `bind_update` | Resolve table, bind SET assignments, WHERE |
| `bind_delete` | Resolve table, bind WHERE |
| `bind_expr` | Recursive expression binding with type checking |
| `resolve_column` | Column name lookup in current scope |

### 5.2 Planner (`src/planner/`)

**Status**: ~15% - All method bodies are `todo!()`

Key methods to implement:

| Method | Purpose |
|--------|---------|
| `plan_select` | Build scan -> filter -> projection tree |
| `plan_insert` | Build InsertNode with child ValuesNode |
| `plan_update` | Build UpdateNode with child scan |
| `plan_delete` | Build DeleteNode with child scan |
| `plan_table_ref` | Produce SeqScanNode or JoinNode |

### 5.3 Optimizer (`src/optimizer/`)

**Status**: ~10% - Minimal passes needed for correctness

Basic passes:
- `push_down_filters` - Move predicates closer to scans
- `push_down_projections` - Eliminate unnecessary columns

---

## 6. Database Bootstrap (HIGH - 0% Complete)

**Location**: Create `src/db.rs` or similar

### 6.1 Database Creation

```rust
pub async fn create_database(path: &Path) -> Result<Database> {
    // 1. Create page directory file (.dir)
    // 2. Create system table heap files
    //    - sys_tables.heap
    //    - sys_columns.heap
    //    - sys_indexes.heap
    // 3. Initialize file headers
    // 4. Bootstrap system table metadata (insert sys_tables into itself, etc.)
}
```

### 6.2 Database Opening

```rust
pub async fn open_database(path: &Path) -> Result<Database> {
    // 1. Open page directory
    // 2. Load system tables
    // 3. Scan sys_tables, sys_columns, sys_indexes into CatalogCache
    // 4. Restore FileId and OID counters from persisted values
}
```

### 6.3 DDL Persistence

Currently `create_table` in `src/accessor/impl.rs` only updates in-memory `CatalogCache`.

Need to add:
```rust
pub async fn create_table(&self, name: &str, columns: &[ColumnDef], txn: &Txn) -> Result<OId> {
    // 1. Allocate OID and FileId
    // 2. Create heap file and initialize header
    // 3. Register in CatalogCache (existing)

    // NEW: Persist to system tables
    // 4. INSERT INTO sys_tables (oid, name, file_id, ...)
    // 5. For each column: INSERT INTO sys_columns (table_oid, col_idx, name, type, ...)

    Ok(oid)
}
```

---

## 7. SQL Pipeline Integration ✅ IMPLEMENTED

**File**: `src/session.rs`

### What's Implemented

The full SQL pipeline is now wired up in `execute_sql_in_txn`:
- Parse SQL → Bind → Plan → Optimize → Execute → Format result
- Transaction handling for BEGIN/COMMIT/ROLLBACK statements
- Auto-commit mode for standalone queries
- Proper async/await support throughout

---

## 8. Recommended Implementation Order

### Phase 1: Make Single-Transaction Queries Work
1. **Data serialization** - `read_field`, `write_field`, `serialize`, `deserialize` in `TupleLayout`
2. **Value operations** - Comparison and arithmetic operators on `Value`
3. **Expression evaluator** - Evaluate `BoundExpr` against tuples
4. **Binder** - Implement core binding methods (start with CREATE TABLE, INSERT, SELECT)
5. **Planner** - Implement core planning methods
6. **Basic Executors** - SeqScan, Filter, Projection, Insert, Values, CreateTable
7. **Wire up SQL pipeline** - Connect Session -> Binder -> Planner -> Executor

### Phase 2: Complete Transaction Support
8. **Transaction commit** - Assign commit_ts, advance global timestamp
9. **Transaction rollback** - Mark aborted, handle cleanup
10. **Active transaction tracking** - For proper snapshot isolation visibility
11. **Enhanced visibility checks** - Use commit timestamps for snapshot isolation

### Phase 3: Database Persistence
12. **Database bootstrap** - Create/open database with system tables
13. **DDL persistence** - Write to sys_tables/sys_columns/sys_indexes
14. **Catalog loading** - Load catalog from system tables on startup

### Phase 4: Complete Feature Set
15. **Remaining executors** - Update, Delete, Join variants, Sort, Limit
16. **Basic optimizer passes** - Filter/projection pushdown
17. **Index operations** - IndexScan executor, CreateIndex with backfill

---

## Files Reference

### Files to Create
```
src/executor/
  mod.rs            - Executor trait and module exports
  seq_scan.rs       - Sequential scan executor
  index_scan.rs     - Index scan executor
  filter.rs         - Filter executor
  projection.rs     - Projection executor
  insert.rs         - Insert executor
  update.rs         - Update executor
  delete.rs         - Delete executor
  values.rs         - Values executor
  join.rs           - Join executors (NLJ, Hash)
  sort.rs           - Sort executor
  limit.rs          - Limit executor
  ddl.rs            - DDL executors (CreateTable, etc.)
  expression.rs     - Expression evaluator

src/db.rs           - Database bootstrap and lifecycle
```

### Files to Modify
```
src/transaction/mod.rs    - Implement commit(), rollback(), active txn tracking
src/databox/databox.rs    - Add Value comparison and arithmetic ops
src/tuple/layout.rs       - Add read_field(), write_field(), serialize(), deserialize()
src/binder/*.rs           - Implement binding methods (all currently todo!())
src/planner/*.rs          - Implement planning methods (all currently todo!())
src/session.rs            - Wire up full SQL pipeline in execute_sql_in_txn()
src/accessor/impl.rs      - Add DDL persistence to system tables
src/accessor/visibility.rs - Enhance for full snapshot isolation
```
