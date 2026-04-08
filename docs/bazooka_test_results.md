# bazooka — test run results

Fresh build, data dir = `./abdb_data`.

## T1 — Fresh bootstrap + sys_tables visibility
```
select * from sys_tables;
→ 3 rows: (0 sys_tables 0) (1 sys_columns 1) (2 sys_indexes 2)
```
**PASS**  (previously only 2 rows showed; oid=0 was missing b/c of the
`frame_buf_mut` UB clobbering the freshly-written row.)

## T2 — Restart without DDL
First run: `Initializing new database...`
Second run: `Loading existing database... Catalog loaded successfully.`
`select * from sys_tables;` returns the same 3 rows.
**PASS**

## T3 — Single-session CREATE + INSERT + SELECT
```
create table users (id int primary key, name string, age smallint);
insert into users values (CAST(1 AS INT), 'john', CAST(20 AS smallint));
select * from users;  →  (1, john, 20)
```
**PASS**

## T4 — Restart persistence of user table
Original behavior: `bind error: unknown table: users`.

First attempt after fix: users showed in sys_tables but accessor didn't register it.
  → **Bug 6**: `load_catalog` used `Txn { id: 0 }`; visibility `xmin(1) <= 0` = false.
  → Fix: use `Txn { id: u64::MAX }` in `load_catalog` ([src/db.rs](src/db.rs)).

Second attempt: users table bound, but select returned 0 rows.
  → **Bug 7**: txn counter reset to 1 on restart; insert row had `xmin=2`,
    new session read with `txn.id=2`, but insert tuples had xmin from txn 2 that
    wasn't visible because of ordering issues — root cause is that txn ids
    aren't persisted across restarts.
  → Fix: added [src/accessor/heap.rs](src/accessor/heap.rs) `max_xmin(bp, file_id)`,
    `load_catalog` returns max xmin seen across sys+user heap files, tcp.rs
    inits `TransactionManager::with_next_txn_id(max + 1)`
    ([src/transaction/mod.rs](src/transaction/mod.rs), [src/server/tcp.rs](src/server/tcp.rs)).

Third attempt: max_xmin came back as 1, not 2. Insert row was still missing.
  → **Bug 8**: INSERT doesn't flush; `kill` doesn't give the server a chance
    either. The users data page with xmin=2 was dirty in memory when killed.
  → Fix: added `AccessorImpl::flush()`; session calls it after every mutation
    ([src/accessor/accessor_impl.rs](src/accessor/accessor_impl.rs), [src/session.rs](src/session.rs)).

Final run:
```
=== RESTART ===
select * from sys_tables;
 → (0 sys_tables 0) (1 sys_columns 1) (2 sys_indexes 2) (1000 users 100)
select * from users;
 → (1, john, 20)
Catalog loaded successfully. max_xmin=2
```
**PASS**

## T5 — Two tables across restart
```
create table users (id int primary key, name string);
create table orders (oid int primary key, total int);
insert into users values (1, 'alice'), (2, 'bob');  [split into two stmts]
insert into orders values (10, 500);
=== RESTART ===
select * from users;   → (1 alice) (2 bob)
select * from orders;  → (10 500)
max_xmin=5
```
**PASS**

## T6 — 50 inserts across restart
```
create table nums (n int primary key, label string);
insert × 50 rows
=== RESTART ===
select * from nums;  → 50 rows
max_xmin=51
```
**PASS**

## Summary
All 6 tests pass. Directory errors are gone. Persistence across restart works
for DDL, INSERT, and multi-table scenarios.

### Limitations still present
- Per-statement `flush_all_dirty` is coarse; real fix is WAL.
- `max_xmin` scan rereads every heap file on startup → O(db size) open time.
- No graceful shutdown hook — still fine because we flush per-statement.
- MVCC visibility for `Txn { id: u64::MAX }` in `load_catalog` bypasses
  commit checks; this is fine as long as we never abort/roll back
  sys-table inserts, which is currently the case.
