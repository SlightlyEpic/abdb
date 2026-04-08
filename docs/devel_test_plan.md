# devel — follow-up test plan

Tests for the devel-branch fixes (§9–12 in `bazooka_fix_log.md`).

Data dir: `./abdb_data`. Server: `cargo run --bin abdb`.

## T1 — build + regression (bazooka T1–T6 still green)
Goal: none of the devel follow-ups broke the earlier persistence work.
```
rm -rf abdb_data
server up
create table users (id int primary key, name string);
insert into users values (CAST(1 AS INT), 'alice');
select * from users;            # → (1 alice)
Ctrl-C                          # graceful shutdown path (new)
server up
select * from users;            # → (1 alice)
```
Pass: row survives.

## T2 — SIGINT mid-session flush
Goal: verify the `tokio::select!` shutdown handler calls `accessor.flush()`
so dirty pages hit disk before exit.
```
rm -rf abdb_data
server up
create table t (id int primary key, v int);
insert into t values (CAST(1 AS INT), CAST(10 AS INT));
kill -INT <pid>                 # NOT kill -9
server up
select * from t;                # → (1 10)
```
Pass: insert survives SIGINT.

## T3 — SIGTERM mid-session flush
Same as T2 but `kill -TERM <pid>`. Pass: insert survives SIGTERM.

## T4 — USER_OID_START filter sanity
Goal: make sure the `oid >= USER_OID_START` refactor still loads user
tables (not just the system catalog).
```
rm -rf abdb_data
server up
create table a (id int primary key);
create table b (id int primary key);
Ctrl-C
server up
select * from sys_tables;       # → sys_tables + sys_columns + sys_indexes + a + b
```
Pass: both user tables present, max_xmin reported.

## T5 — Binder dead-code removal compiles + runs join
Goal: the removed `let join_kind` didn't break join binding.
```
rm -rf abdb_data
server up
create table u (id int primary key, name string);
create table o (id int primary key, uid int);
insert into u values (CAST(1 AS INT), 'a');
insert into o values (CAST(10 AS INT), CAST(1 AS INT));
select u.name, o.id from u join o on u.id = o.uid;
```
Pass: query returns `(a, 10)`.
