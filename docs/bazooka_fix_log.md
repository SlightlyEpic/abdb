# bazooka branch — directory-error fix log

## Current status
All 6 test cases (T1–T6) pass. DDL, INSERT, and multi-table persistence
survive server restart. See [bazooka_test_results.md](bazooka_test_results.md).

## Fixes already applied

### 1. `BTreePageDirectory` was never flushed to disk
Added `DiskManager::flush_metadata` → forwards to `page_directory.flush_all_dirty()`.
`BufferPool::flush_all_dirty` now calls it at the end so every page flush also persists the directory.
- [src/storage/disk.rs](src/storage/disk.rs): trait method + `DiskManagerImpl` impl
- [src/buffer/impl/buffer_pool.rs](src/buffer/impl/buffer_pool.rs): `flush_all_dirty` chains `disk_manager.flush_metadata()`

### 2. Fresh heap files relied on silent EOF→zeros kludge
Reverted the EOF→zeros branch in `read_page_at_loc` ([src/storage/disk.rs](src/storage/disk.rs)).
Added `DiskManager::init_page_at_loc` — allocates an LPageId, registers it in the
page directory, and writes a zero page to disk so subsequent reads succeed.
Exposed via `BufferPool::init_page_at_loc` trait method.

### 3. Heap file header page_id was hardcoded to 0
Every heap-file header shared `lpage_id = 0` → `frame_lpage_id_map` collision.
- [src/db.rs](src/db.rs) `init_heap_file`: now calls `bp.init_page_at_loc(header_loc)` and
  passes the returned real lpage_id into `HeapFileHeaderPage::init`.
- [src/accessor/accessor_impl.rs](src/accessor/accessor_impl.rs) `create_table`: same treatment.
  Also calls `bp.flush_all_dirty()` at the end so DDL is durable before returning.

### 4. `frame_buf_mut` / `frame_buf` pointed at the AlignedBuffer struct, not its data
Pre-existing UB/memory-corruption bug. `self.buf.get() as *mut u8` yielded a pointer
to the `AlignedBuffer` struct fields (`ptr`, `layout`, `len`) instead of the heap-
allocated aligned memory. Every page read/write was trampling ~4KB of adjacent heap.
The old EOF→zeros kludge had been hiding it by skipping `pread` on empty files.
- [src/buffer/impl/buffer_pool.rs:246-262](src/buffer/impl/buffer_pool.rs#L246-L262):
  `(*self.buf.get()).as_ptr()` instead of `self.buf.get() as *mut u8`.
This is the explanation for the earlier `malloc: Corruption of tiny freelist` reports.

### 5. Dead import + unused `mut`
- [src/server/tcp.rs:2](src/server/tcp.rs#L2): removed `use sqlparser::keywords::FORMAT;`
- [src/accessor/accessor_impl.rs](src/accessor/accessor_impl.rs): `let mut tuple_bytes` → `let tuple_bytes`

### 6. `load_catalog` used `Txn { id: 0 }` — dropped every user row
Visibility check `xmin <= txn.id` → `1 <= 0` = false, so any sys_tables row
written by a prior session (xmin ≥ 1) was invisible to the loader. Fixed by
using `Txn { id: u64::MAX, Snapshot }` in [src/db.rs](src/db.rs).
**Caveat:** this bypasses commit-status checks entirely. Safe today because
bootstrap/DDL inserts never roll back, but will need a proper
`read_everything_committed` txn once rollback exists.

### 7. Txn counter reset to 1 on every restart
`TransactionManager::new()` hardcoded `next_txn_id = 1`. After restart, the
new session got `txn.id = 1`, but on-disk rows already had `xmin = 2, 3, ...`
from the previous session → visibility filter dropped them all.
- Added `heap::max_xmin(bp, file_id)` in [src/accessor/heap.rs](src/accessor/heap.rs)
  (walks heap pages, returns max xmin across all live tuples).
- `db::load_catalog` now scans every sys + user heap file for max xmin and
  returns it.
- `TransactionManager::with_next_txn_id(id)` constructor in
  [src/transaction/mod.rs](src/transaction/mod.rs).
- [src/server/tcp.rs](src/server/tcp.rs) inits the manager with `max_xmin + 1`.

### 8. INSERT didn't flush → data lost on `kill`
Only `create_table` flushed. A row inserted via INSERT sat in a dirty frame
until the next eviction or `flush_all_dirty` — a `kill` mid-session destroyed it.
- Added `AccessorImpl::flush()` in [src/accessor/accessor_impl.rs](src/accessor/accessor_impl.rs)
  (forwards to `bp.flush_all_dirty`, which chains `flush_metadata`).
- [src/session.rs](src/session.rs) calls `accessor.flush()` after every
  execution that produced `RowsAffected` or `Ok(msg)` — i.e. every mutation
  or DDL — so committed state is durable before the next SQL round-trip.
- Coarse (whole buffer pool per statement); proper fix is WAL.

---

## Open / latent issues not fixed on this branch

### Correctness / durability
- **No graceful shutdown.** SIGTERM/SIGINT drop anything dirty mid-statement
  (e.g. a long-running INSERT ... SELECT). Per-statement flush minimizes
  exposure but doesn't eliminate it. Should install a `tokio::signal` handler
  in `main.rs` that calls `accessor.flush()` before exit.
- **`load_catalog` bypass is a hack.** `txn.id = u64::MAX` sees every tuple
  regardless of commit status. Fine today, breaks the moment transactions
  can abort.
- **`max_xmin` scan is O(db).** Every heap page read at startup. Fine for
  small DBs, bad for anything real. Persist the counter to a metadata file
  and update periodically.
- **Marker file written before we know anything actually persisted.**
  [src/server/tcp.rs](src/server/tcp.rs) writes `.abdb_init_marker` right
  after `bootstrap_database` returns. If the next flush-on-statement fails,
  the DB is stuck in "exists but empty" state.

### Latent bugs (not observable today)
- **`fetch_page_at_loc_write` still reads `lpage_id` from a zero buffer.**
  The path for a freshly-zeroed page reads `uber_header.page_id = 0` and
  publishes `frame_lpage_id_map[0] = frame` before the caller overwrites the
  header with its real ID. `FrameMeta::Loaded { lpage_id: 0 }` is wrong.
  Every fresh heap header goes through this path on create. Currently
  invisible because (a) writeback uses ppage_id, and (b) nothing does
  `fetch_page_write(0)` logically. Will bite anyone who adds a code path
  that does.
  Fix: either take an explicit `lpage_id` hint in `fetch_page_at_loc_write`
  (caller passes the ID it just allocated via `init_page_at_loc`), or add
  a `set_lpage_id(frame_idx, id)` method the caller uses after writing the
  real header.

### Architectural hygiene
- **`heap::{scan,insert,get,delete,max_xmin}` are now `pub`.** Was
  `pub(super)`. Bumped so `db.rs` can call them directly. Accessor
  encapsulation is broken — `db.rs` should go through the `Accessor` trait
  or move into the `accessor` module.
- **Duplicated DDL logic.** `AccessorImpl::create_table` duplicates
  `db::persist_create_table` + `insert_sys_{table,column}_record`. Two
  encoders for the same tuple → drift hazard. Pick one.
- **`storage/disk.rs::get_file` silently defaults unknown file_ids to
  `FileType::Heap`** ([src/storage/disk.rs:179](src/storage/disk.rs#L179)).
  `create_index` will open `.heap` instead of `.idx` if caller forgets to
  call `register_file`. Should return an error.
- **`executor/execute.rs` is 1113 lines**, over the 500-line guideline in
  [CLAUDE.md](CLAUDE.md). Split by operator.
- **Pre-existing: `binder/binder.rs` borrow-after-move ~L469** on
  `join.kind`. Unrelated to persistence but tripping on the bazooka branch.

### Minor
- Welcome/prompt writing in [src/server/tcp.rs](src/server/tcp.rs) is
  inconsistent — empty input writes `"abdb>"` without newline, non-empty
  writes `"\n...\n\nabdb> "`.
- `load_catalog` filter `oid > SYS_TABLE_INDEXES_OID` should be
  `oid >= USER_OID_START` for clarity (user OIDs start at 1000 per
  `SessionOidAllocator`).

---

## Test plan (AWAITING YOUR PERMISSION BEFORE RUNNING)

Each test runs against a fresh `./abdb_data` directory (the real data dir, per
`src/main.rs` — not `./data`).

### T1 — Fresh bootstrap + system-table visibility
Goal: prove bootstrap writes directory + pages to disk correctly.
```
rm -rf abdb_data
make abdb &   # run server in background
# connect via nc, run:
select * from sys_tables;
# expect: 3 rows (sys_tables oid=0, sys_columns oid=1, sys_indexes oid=2)
# currently only 2 rows show — oid=0 missing. Possibly a visibility/MVCC issue.
```
Pass criteria: all 3 sys_tables rows visible.

### T2 — Restart without any DDL
Goal: verify bootstrap state round-trips through restart.
```
rm -rf abdb_data
make abdb     # first run: "Initializing new database..."
Ctrl-C
make abdb     # second run: "Loading existing database... Catalog loaded successfully."
# connect, run: select * from sys_tables;
```
Pass criteria: second run does not panic; sys_tables contents match first run.

### T3 — Single-session CREATE + INSERT + SELECT
Goal: confirm current working path.
```
rm -rf abdb_data
make abdb
# connect via nc:
create table users (id int primary key, name string, age smallint);
insert into users (id, name, age) values (CAST(1 AS INT), 'john', CAST(20 AS smallint));
select * from users;
```
Pass criteria: SELECT shows the inserted row.

### T4 — Restart persistence of user tables  ← the currently failing case
Goal: after T3, restart server, check user data is still there.
```
# after T3 completed:
Ctrl-C
make abdb       # should print "Loading existing database..."
# reconnect via nc:
select * from sys_tables;  # expect users row present
select * from users;       # expect { id=1, name=john, age=20 }
```
Current behavior: `bind error: unknown table: users`.
Pass criteria: users row shows in sys_tables, SELECT returns the row.

### T5 — Two separate tables across restart
Same as T4 but create two tables before restart, insert into both, restart, read both.

### T6 — Multiple inserts then restart
Create one table, insert ~50 rows (forcing multiple heap pages), restart, count.
Pass criteria: all 50 rows present after restart.

### T7 — DROP TABLE persistence (if DROP is wired up)
Skip if not implemented on this branch.

---

## Debugging hypotheses for T4 failure (to check if T4 fails)

1. **Runtime `create_table` doesn't actually flush sys_tables header page.**
   The fix added `bp.flush_all_dirty()` at the end of `create_table`, but the
   sys_tables *header page* update (the `first_page` pointer change) happens
   inside `heap::insert` via `bp.fetch_page_at_loc_write(header_loc)`. Need to
   confirm that guard's `mark_dirty()` is actually called — check
   [src/accessor/heap.rs:232-241](src/accessor/heap.rs#L232-L241).

2. **`load_catalog` filter off by one.**
   [src/db.rs](src/db.rs) filters `oid > SYS_TABLE_INDEXES_OID` (= 2). User OIDs
   start at 1000, so filter should pass. But worth inspecting the raw scan
   output to confirm the row is actually on disk.

3. **MVCC visibility rejects the row on load.**
   Both bootstrap and load_catalog use `Txn { id: 0, Snapshot }`. If snapshot
   visibility requires `xmin < read_ts` and `read_ts = 0`, rows inserted with
   `xmin = 0` (bootstrap) and `xmin = 1` (first session's CREATE TABLE txn)
   might fail visibility check in different ways depending on the rule.

4. **Page directory still isn't flushed on runtime DDL.**
   `create_table` calls `bp.flush_all_dirty()` which chains `flush_metadata()`.
   But `flush_metadata` only flushes the directory's *dirty set*; if directory
   writes during `heap::insert`→`new_page`→`add_page` don't mark the directory
   page dirty, they wouldn't be flushed. Worth checking
   [src/storage/directory/directory.rs](src/storage/directory/directory.rs)
   `add_page` → does it add to the dirty set?

5. **`fetch_page_at_loc_write` bug: lpage_id=0 still leaking.**
   Even with the header now having a real lpage_id stored in its uber header,
   the buffer pool's fetch_page_at_loc_write *still* reads the uber header from
   the zero page that `init_page_at_loc` just wrote — at that moment the uber
   header is still all zeros, so the frame is published into
   `frame_lpage_id_map[0]` before the caller overwrites with the real id. The
   frame's `FrameMeta::Loaded { lpage_id }` also gets stuck at 0. Subsequent
   `fetch_page_write(real_lpage)` won't find the frame → loads it again from
   disk → there are now two frames for the same page, and the evicted one can
   clobber the newer one.
   Fix would require either (a) having `fetch_page_at_loc_write` accept an
   explicit lpage_id hint, or (b) adding a `set_lpage_id(frame_idx, id)` call
   after the caller writes the header.

If you green-light the test plan I will run T1–T6 in order, recording each
command and its output in `docs/bazooka_test_results.md`, and stop at the
first failure to diagnose.
