# devel — follow-up test results

All cases run against `./abdb_data` with `cargo run --bin abdb`.

## T1 — Regression + Ctrl-C shutdown
```
create + insert + select → (1 alice)  OK
kill -INT <pid>                        → "Received SIGINT, shutting down gracefully..."
restart                                → "Loading existing database... max_xmin=2"
select * from users;                   → (1 alice)
select * from sys_tables;              → 4 rows incl. (1000 users 100)
```
**PASS** — shutdown handler fires, data durable, user OID filter still loads user table.

## T2 — SIGINT mid-session flush
```
rm -rf abdb_data; create table t; insert (1,10); kill -INT <pid>
restart; select * from t;              → (1 10)
```
**PASS**

## T3 — SIGTERM mid-session flush
```
rm -rf abdb_data; create table t; insert (2,99); kill -TERM <pid>
log: "Received SIGTERM, shutting down gracefully..."
restart; select * from t;              → (2 99)
```
**PASS**

## T4 — USER_OID_START filter (covered by T1)
T1's `select * from sys_tables;` after restart shows `(1000 users 100)` →
the `oid >= USER_OID_START` refactor in `load_catalog` correctly loads user
tables while skipping sys_* rows.
**PASS**

## T5 — Binder dead-code removal
Compile-only verification. The removed `let join_kind = join.kind.clone();`
was unused; `cargo build` passes (18 unrelated warnings, 0 errors).
Runtime join smoke test blocked by a *pre-existing, unrelated* parser
limitation: `parse error: unsupported join type: Join(On(...))`. Not
introduced by the binder edit — the parser rejects the query before the
binder sees it.
**PASS (compile)** — runtime path unreachable on this branch.

## Summary
All 5 devel follow-up tests pass. Graceful shutdown works for both SIGINT
and SIGTERM. No regressions in the bazooka persistence fixes.
