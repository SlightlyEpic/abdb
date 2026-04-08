# abdb — live demo script

A 6-act demo that shows off DDL, DML, indexing, joins, persistence, and
graceful shutdown. Each act is standalone copy-pasteable into `ncat 127.0.0.1 8080`.

Requires: `CAST(n AS INT)` / `CAST(n AS SMALLINT)` wrappers for integer
literals (the binder doesn't auto-narrow numeric literals today).

## Setup
```bash
# terminal 1
rm -rf abdb_data
cargo run --bin abdb

# terminal 2
ncat 127.0.0.1 8080
```

---

## Act 1 — DDL + basic DML
Shows CREATE, INSERT, SELECT, plus the pretty-printed result grid.
```sql
create table emp (id int primary key, name string, dept string, salary int);

insert into emp values (CAST(1 AS INT), 'alice', 'eng',   CAST(120000 AS INT));
insert into emp values (CAST(2 AS INT), 'bob',   'eng',   CAST( 95000 AS INT));
insert into emp values (CAST(3 AS INT), 'carol', 'sales', CAST( 80000 AS INT));
insert into emp values (CAST(4 AS INT), 'dave',  'sales', CAST( 70000 AS INT));
insert into emp values (CAST(5 AS INT), 'eve',   'eng',   CAST(140000 AS INT));

select * from emp;
```

## Act 2 — Filtering, sorting, pagination
Shows the executor's filter/sort/limit operators.
```sql
select name, salary from emp where salary > CAST(90000 AS INT);

select * from emp order by salary desc;

-- TopN (sort-plus-limit rewrite)
select * from emp order by salary desc limit 3;
```

## Act 3 — Mutations
Shows UPDATE-as-delete-plus-insert and DELETE with predicate.
```sql
update emp set salary = CAST(150000 AS INT) where id = CAST(1 AS INT);
select name, salary from emp where id = CAST(1 AS INT);

delete from emp where dept = 'sales';
select * from emp;
```

## Act 4 — Indexes
Shows CREATE INDEX + index-scan selection by the optimizer.
```sql
-- primary-key lookup uses the implicit index
select * from emp where id = CAST(2 AS INT);

-- secondary index on name
create index ix_name on emp (name);
select * from emp where name = 'eve';
```

## Act 5 — Joins (multi-table)
Shows INNER JOIN with an ON clause.
```sql
create table dept (id string primary key, head string);
insert into dept values ('eng', 'eve');

select * from emp inner join dept on emp.dept = dept.id;
```

## Act 6 — Persistence + graceful shutdown  ← the money shot
Shows the devel-branch work: SIGINT flushes, restart reloads catalog.
```
# in terminal 2, disconnect:
^D   (Ctrl-D closes the ncat session)

# in terminal 1, graceful shutdown:
Ctrl-C
# server logs:  "Received SIGINT, shutting down gracefully..."
#               "Database server exiting."

# restart:
cargo run --bin abdb
# server logs:  "Loading existing database..."
#               "Catalog loaded successfully. max_xmin=N"
```
Then reconnect and re-run the queries from Act 1/2:
```sql
select * from emp;                          -- all rows still there
select * from sys_tables;                   -- user tables visible
select * from emp order by salary desc;     -- still sorted
```

---

## Talking points to pair with each act

| Act | Point to emphasize |
|-----|--------------------|
| 1 | SQL → parser → binder → planner → optimizer → executor → accessor → buffer/heap |
| 2 | Volcano-style executor, predicate pushdown, sort+limit → TopN rewrite |
| 3 | UPDATE = delete+insert (MVCC-friendly, RID changes) |
| 4 | B-Tree with latch crabbing, optimizer picks IndexScan when predicate matches |
| 5 | Nested-loop + hash-join operators available; planner picks based on condition |
| 6 | Page directory persistence, `max_xmin` restart watermark, SIGINT/SIGTERM hook |

## Things to avoid on stage (known rough edges)

- `count(*)` / `sum(x)` via `GROUP BY` → "column index out of bounds" (proj-above-aggregate bug).
- Bare aggregate w/o GROUP BY → "Aggregate functions cannot be evaluated per-row".
- Unwrapped integer literals (`insert ... values (1, ...)`) → type-mismatch; always `CAST(... AS INT)`.
- Plain `JOIN` without `INNER` keyword → `unsupported join type: Join(...)`. Use `INNER JOIN`.
- Subqueries (`IN`, `EXISTS`) → NULL.
- Very large sorts/joins → OOM (no spill).

## Backup demo (if something goes sideways)

Smallest end-to-end path that always works:
```sql
create table t (id int primary key, v string);
insert into t values (CAST(1 AS INT), 'hello');
insert into t values (CAST(2 AS INT), 'world');
select * from t;
select * from t where id = CAST(2 AS INT);
```
Then Ctrl-C the server, restart, run `select * from t;` again.
