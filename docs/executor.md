# Executor — In-Memory Materialized Plan Interpreter

> Memory-only. No disk spilling. All intermediate results live in `Vec<Row>`.

---

## Architecture

The executor sits between the optimizer (which produces a `PhysicalPlan` tree) and the
accessor layer (which provides tuple-level storage operations). It walks the physical plan
tree recursively, materializing each operator's output fully into memory before passing
it to the parent.

```
PhysicalPlan tree
       |
       v
   Executor<A: Accessor>
       |
       +--- eval::eval_expr()    — expression evaluator
       +--- codec::decode_row()  — raw tuple bytes -> Vec<Value>
       +--- codec::encode_row()  — Vec<Value> -> raw tuple bytes
       |
       v
   Accessor (async table/index I/O)
```

### Design Choice: Materialized vs Volcano

The classic Volcano model pulls one tuple at a time via `next()`. This executor instead
**materializes** each operator's full output into `Vec<Row>` before returning to the parent.
This simplifies the implementation (no iterator state machines, no complex lifetime management
across async boundaries) and is a natural fit for the "no disk spilling" constraint — everything
is already in memory by definition.

Trade-off: higher peak memory usage since every intermediate result is fully buffered. For a
production system, a streaming Volcano model with spill-to-disk would be needed for large
datasets. For the current scope this is the right call.

---

## Module Layout

```
src/executor/
  mod.rs        — Row type alias, ExecError, ExecResult
  eval.rs       — BoundExpr evaluator (pure, no I/O)
  codec.rs      — Tuple encode/decode via TupleLayout
  executor.rs   — Executor struct + all operator implementations
```

---

## Types

```rust
pub type Row = Vec<Value>;           // A single decoded tuple
pub type ExecResult<T> = Result<T, ExecError>;

pub enum ExecError {
    Accessor(accessor::Error),       // Storage-layer failure
    Eval(String),                    // Type mismatch, division by zero, etc.
    Internal(String),                // Bug: should-not-happen states
}
```

`Executor::execute()` returns `ExecResult<Vec<Row>>`. The caller (session layer) can
pair this with the plan's `schema()` to get column names/types for display.

---

## Operator Coverage

Every variant of `PhysicalPlan` is handled:

| # | PhysicalPlan variant | Executor method | Strategy |
|---|---------------------|-----------------|----------|
| 1 | `SeqScan` | `exec_seq_scan` | Stream table via accessor, decode each tuple through `TupleLayout`, apply pushed predicates |
| 2 | `IndexScan` | `exec_index_scan` | Range-scan B-tree index, fetch each tuple by RID via `table_get`, apply residual predicates |
| 3 | `Filter` | `exec_filter` | Execute child, retain rows where predicate evaluates to `true` |
| 4 | `Projection` | `exec_projection` | Execute child, evaluate each output expression per row. Special handling for aggregate functions: resolved by alias lookup in input schema |
| 5 | `NestedLoopJoin` | `exec_nl_join` | O(N*M) cross-product with condition check. Supports Inner, Left/Right/Full Outer via matched-row tracking |
| 6 | `HashJoin` | `exec_hash_join` | Build hash table on right side keyed by `right_keys`, probe with left side's `left_keys`. Residual predicate applied post-probe. All four join kinds supported |
| 7 | `HashAggregate` | `exec_hash_agg` | Group-by key hashing into `HashMap<key, (group_vals, Vec<Accumulator>)>`. Scalar aggregates on empty input return one row (e.g., `COUNT(*)` = 0, `SUM(x)` = NULL) |
| 8 | `StreamAggregate` | `exec_hash_agg` | Delegates to hash aggregate (functionally equivalent when fully materialized) |
| 9 | `Sort` | `exec_sort` | In-memory `sort_by` with null-aware comparator respecting ASC/DESC and NULLS FIRST/LAST |
| 10 | `TopN` | `exec_topn` | Full sort + skip(offset) + take(limit). Equivalent to Sort+Limit fused by optimizer |
| 11 | `Limit` | `exec_limit` | skip(offset) + take(limit) on child output |
| 12 | `Distinct` | `exec_distinct` | HashSet-based deduplication using serialized row keys |
| 13 | `HashDistinct` | `exec_distinct` | Same as Distinct (both use hash-based dedup) |
| 14 | `Insert` | `exec_insert` | Execute source, map target columns to full table layout, encode via `TupleLayout`, call `table_insert`. Returns `[rows_affected]` |
| 15 | `Update` | `exec_update` | Scan with RIDs, evaluate assignment expressions, delete old tuple + insert new tuple. Returns `[rows_affected]` |
| 16 | `Delete` | `exec_delete` | Scan with RIDs, call `table_delete` for each. Returns `[rows_affected]` |
| 17 | `Values` | `exec_values` | Evaluate each expression row from the literal value list |
| 18 | `CreateTable` | — | Returns empty (DDL side-effects handled by session/accessor layer) |
| 19 | `DropTable` | — | Returns empty |
| 20 | `AlterTable` | — | Returns empty |
| 21 | `CreateIndex` | — | Returns empty |
| 22 | `DropIndex` | — | Returns empty |
| 23 | `Nothing` | — | Returns empty (transaction control: BEGIN/COMMIT/ROLLBACK) |

---

## Expression Evaluator (`eval.rs`)

`eval_expr(expr: &BoundExpr, row: &[Value]) -> Value`

Pure function — no I/O, no side effects. Takes a bound expression tree and the current
row (indexed by `scope_index`) and returns a `Value`.

### Supported Expression Kinds

| Kind | Behavior |
|------|----------|
| `Literal` | Integer -> I64, Float -> F64, String, Bool, Null |
| `ColumnRef` | `row[scope_index].clone()` |
| `BinaryOp` | Arithmetic (+, -, *, /, %), comparison (=, !=, <, <=, >, >=), logical (AND, OR with short-circuit), string concat (||) |
| `UnaryOp` | Negation (numeric), NOT (boolean) |
| `IsNull` / `IsNotNull` | Null check, returns Bool |
| `Like` | SQL LIKE with `%` (any sequence) and `_` (any char). Iterative backtracking algorithm |
| `Between` | `expr >= low AND expr <= high`, with optional negation |
| `In` | Membership test against literal list, with optional negation |
| `Function` | Scalar: COALESCE, NULLIF, UPPER, LOWER, LENGTH, ABS. Aggregates return Null (handled by aggregate operator) |
| `Cast` | Delegates to `Value::cast(target_type)` |
| `Case` | Simple CASE (match operand) and searched CASE (evaluate conditions). Falls through to ELSE or Null |
| `InSubquery` / `Exists` / `Subquery` | Not supported (returns Null). Would require correlated subquery execution |

### NULL Propagation

Follows SQL three-valued logic:
- Arithmetic/comparison with NULL -> NULL
- `AND`: FALSE AND NULL -> FALSE, TRUE AND NULL -> NULL
- `OR`: TRUE OR NULL -> TRUE, FALSE OR NULL -> NULL
- Short-circuit: AND stops on FALSE, OR stops on TRUE

---

## Tuple Codec (`codec.rs`)

### `decode_row(layout, columns, raw_bytes) -> Row`

Reads a raw tuple buffer (including the 16-byte MVCC header) and produces a `Vec<Value>`.
Uses `TupleLayout::read_field` for each column, respecting the null bitmap.

Columns must be in position order (as stored in the catalog).

### `encode_row(layout, columns, values) -> Vec<u8>`

Produces a raw tuple buffer from a `Vec<Value>`. Two-pass:
1. Fixed-width values written at their layout offsets via `TupleLayout::write_field`
2. Variable-length strings: data appended after `fixed_len`, pointer (u16 len + u16 offset) written at the column's fixed offset

MVCC header (XMIN/XMAX) is zeroed — the accessor/heap layer sets these on insert.

---

## Join Implementation Details

### Nested Loop Join

```
for each left_row:
    for each right_row:
        combined = left_row ++ right_row
        if condition(combined): emit
    if no_match and (LEFT or FULL): emit left_row ++ NULLs
for each unmatched right_row (RIGHT or FULL):
    emit NULLs ++ right_row
```

### Hash Join

```
BUILD: hash right_rows by right_keys -> HashMap<key, Vec<(index, row)>>
PROBE: for each left_row:
    key = hash(left_keys)
    for each match in table[key]:
        combined = left_row ++ right_row
        if residual(combined): emit, mark right_row matched
    if no_match and (LEFT or FULL): emit left_row ++ NULLs
UNMATCHED: for each unmatched right_row (RIGHT or FULL):
    emit NULLs ++ right_row
```

Right side is always the build side (chosen by the optimizer during equi-join extraction).

---

## Aggregate Implementation Details

### Accumulator

Each aggregate function gets an `Accumulator` instance per group:

| Function | State | `feed(val)` | `finish()` |
|----------|-------|-------------|------------|
| COUNT | `count: i64` | If has_arg: skip nulls. Else: count all | I64(count) |
| SUM | `sum: f64, count: i64` | Skip nulls, add to_f64 | F64(sum) or Null if count=0 |
| AVG | `sum: f64, count: i64` | Skip nulls, add to_f64 | F64(sum/count) or Null if count=0 |
| MIN | `min_val: Option<Value>` | Skip nulls, track minimum via PartialOrd | min_val or Null |
| MAX | `max_val: Option<Value>` | Skip nulls, track maximum via PartialOrd | max_val or Null |

**DISTINCT aggregates**: Each accumulator maintains a `HashSet<Vec<u8>>` of seen value
byte representations. Duplicate values are skipped before feeding.

**Scalar aggregates on empty input**: When there are no group-by keys and the input is
empty, one row is still emitted (e.g., `SELECT COUNT(*) FROM empty_table` returns `0`,
`SELECT SUM(x) FROM empty_table` returns `NULL`).

---

## DML: Record ID Tracking

UPDATE and DELETE need the physical `RecordId` (page_id + slot_id) of each tuple they
modify. The generic `exec()` path discards RIDs since query operators don't need them.

`scan_with_rids(plan) -> Vec<(Row, RecordId)>` is a specialized execution path that
preserves RIDs through the scan chain:

- `SeqScan` -> pairs each decoded row with the RID from the heap stream
- `IndexScan` -> pairs each fetched tuple with the RID from the index
- `Filter` -> passes through `(row, rid)` pairs, filtering by predicate

Other plan nodes (joins, projections, etc.) are rejected — DML inputs are always
scan-based in the current planner.

---

## Row Hashing

For HashJoin, HashAggregate, and Distinct, rows (or subsets of row values) need to be
used as HashMap keys. Since `Value` doesn't implement `Hash` (due to floats), we serialize
values to a deterministic byte key:

```
For each value:
  Null       -> [0x00]
  Non-null   -> [type_tag] [length_prefix if String] [to_bytes()]
```

Type tags prevent collisions between different types with the same byte representation.
Length prefixes prevent ambiguous concatenation of variable-length values.

---

## Projection Above Aggregate

The current planner does not rewrite aggregate function expressions in SELECT lists to
ColumnRef nodes after inserting an Aggregate node. For example:

```sql
SELECT a, COUNT(*) FROM t GROUP BY a
```

The projection's expressions are `[ColumnRef(a), Function(Count)]` — the `COUNT(*)` is
still a Function node, not a ColumnRef pointing into the aggregate output.

The executor handles this by checking each projection expression: if it's a top-level
aggregate function, it resolves the value by matching the projection's alias against the
input (aggregate output) schema column names. This works because `collect_aggregates` in
the planner uses the same alias for both the AggregateExpr and the projection.

Limitation: compound expressions containing aggregates (e.g., `COUNT(*) + 1`) are not
rewritten. The aggregate function buried inside a BinaryOp will evaluate to Null. A proper
fix belongs in the binder/planner (rewrite aggregate references to ColumnRef after aggregate
insertion).

---

## Limitations

| Area | Limitation | Path to Fix |
|------|-----------|-------------|
| Memory | No spill-to-disk for sort, hash join, or aggregation | External sort, grace hash join |
| DDL | Executor returns empty for DDL; actual catalog mutation is caller's job | Wire DDL methods on Accessor trait into executor |
| Subqueries | `IN (SELECT ...)`, `EXISTS`, scalar subquery expressions return Null | Correlated subquery execution in eval_expr |
| Aggregate expressions | `COUNT(*) + 1` in SELECT doesn't work above an aggregate node | Planner rewrite of agg refs to ColumnRef |
| Index backfill | `CREATE INDEX` doesn't scan existing data into the new index | Executor should scan table + call index_insert |
| TopN | Uses full sort + take instead of a heap-based partial sort | BinaryHeap-based bounded sort |
| Update strategy | Delete-old + insert-new (may change RID) | In-place update when tuple fits in same slot |
