# abdb — manual test queries

This file lists the SQL snippets used to exercise the engine end‑to‑end on the
`fixes` branch. It mirrors the regression pass run after the latest round of
fixes (persistence, subqueries, IN‑list coercion, F32 literal coercion,
DROP INDEX without `ON`, IF NOT EXISTS dedupe, foreign key enforcement, FK
CASCADE / SET NULL).

## Harness

`src/bin/client.rs` is empty, so the tests drive the server over TCP directly.

Start the server:

```bash
rm -rf abdb_data
RUSTFLAGS=-Awarnings cargo build --bin abdb
nohup ./target/debug/abdb > /tmp/abdb.log 2>&1 &
```

A minimal Python client that speaks the line‑oriented protocol
(`welcome` + `abdb> ` prompt after each query) lives at `/tmp/abdb_test.py`:

```python
#!/usr/bin/env python3
import socket, sys
HOST, PORT, PROMPT = '127.0.0.1', 8080, b'abdb> '

def recv_until_prompt(s, timeout=5):
    s.settimeout(timeout)
    buf = b''
    while PROMPT not in buf:
        chunk = s.recv(4096)
        if not chunk:
            break
        buf += chunk
    if buf.endswith(PROMPT):
        buf = buf[:-len(PROMPT)]
    return buf.decode(errors='replace')

def run(queries):
    s = socket.socket()
    s.connect((HOST, PORT))
    recv_until_prompt(s)
    for q in queries:
        s.sendall((q + '\n').encode())
        out = recv_until_prompt(s).strip()
        print(f'> {q}\n{out}\n')
    s.close()

if __name__ == '__main__':
    src = open(sys.argv[1]) if len(sys.argv) > 1 else sys.stdin
    run([l.strip() for l in src if l.strip() and not l.strip().startswith('#')])
```

Usage:

```bash
cat queries.sql | python3 /tmp/abdb_test.py
# or
python3 /tmp/abdb_test.py queries.sql
```

---

## 1. DDL + constraints

```sql
CREATE TABLE t1 (id INT PRIMARY KEY, name VARCHAR NOT NULL, age INT DEFAULT 18, email VARCHAR UNIQUE);
INSERT INTO t1 (id, name) VALUES (1, 'alice');                    -- ok; age defaults to 18
INSERT INTO t1 VALUES (2, 'bob', 30, 'bob@x');                    -- ok
INSERT INTO t1 VALUES (1, 'dup', 20, 'd@x');                      -- err: PK violation on id
INSERT INTO t1 (id, name, email) VALUES (3, 'carol', 'bob@x');    -- err: UNIQUE violation on email
INSERT INTO t1 (id, age) VALUES (4, 25);                          -- err: NOT NULL on name
SELECT * FROM t1;                                                 -- {(1,alice,18,NULL),(2,bob,30,bob@x)}

SHOW TABLES;
DESC t1;
```

## 2. Indexes

```sql
CREATE INDEX idx_age ON t1(age);
DROP INDEX idx_age;        -- no ON-clause required anymore
DROP INDEX idx_age ON t1;  -- still accepted for sqlparser compatibility
```

## 3. Joins

```sql
CREATE TABLE t2 (id INT PRIMARY KEY, t1_id INT, v INT);
INSERT INTO t2 VALUES (1, 1, 100);
INSERT INTO t2 VALUES (2, 2, 200);
INSERT INTO t2 VALUES (3, 1, 300);

SELECT t1.name, t2.v FROM t1 INNER JOIN t2 ON t1.id = t2.t1_id;
SELECT t1.name, t2.v FROM t1 LEFT  JOIN t2 ON t1.id = t2.t1_id;
SELECT t1.name, t2.v FROM t1 RIGHT JOIN t2 ON t1.id = t2.t1_id;
SELECT t1.name, t2.v FROM t1 FULL OUTER JOIN t2 ON t1.id = t2.t1_id;
```

## 4. Aggregates, GROUP BY, HAVING

```sql
SELECT COUNT(*), SUM(v), AVG(v), MIN(v), MAX(v) FROM t2;
SELECT t1_id, COUNT(*), SUM(v) FROM t2 GROUP BY t1_id;
SELECT t1_id, COUNT(*) FROM t2 GROUP BY t1_id HAVING COUNT(*) > 1;
```

## 5. DML + WHERE

```sql
UPDATE t1 SET age = 99 WHERE id = 1;
SELECT * FROM t1 WHERE age = 99;
DELETE FROM t2 WHERE v = 300;
SELECT * FROM t2;
```

## 6. ALTER TABLE

```sql
ALTER TABLE t1 ADD COLUMN city VARCHAR DEFAULT 'nyc';
ALTER TABLE t1 DROP COLUMN email;
SELECT * FROM t1;

ALTER TABLE t1 ADD COLUMN bad INT NOT NULL;              -- err: NOT NULL without DEFAULT on populated table
ALTER TABLE t1 ADD COLUMN ok  INT NOT NULL DEFAULT 0;    -- ok
SELECT * FROM t1;
```

## 7. IF (NOT) EXISTS

```sql
CREATE TABLE IF NOT EXISTS t1 (id INT);         -- "skipped: already exists"
CREATE TABLE IF NOT EXISTS brand_new (id INT);  -- ok
DROP TABLE IF EXISTS brand_new;                 -- ok
DROP TABLE IF EXISTS nonexistent;               -- silent ok
```

## 8. Data types other than INT

```sql
CREATE TABLE tb (b BOOLEAN, f REAL, s VARCHAR(10), bi BIGINT);
INSERT INTO tb VALUES (true, 3.14, 'hello', 9999999999);  -- F64 literal now coerces into REAL/F32
INSERT INTO tb VALUES (false, 2.71, 'world', 1);
SELECT * FROM tb;
SELECT * FROM tb WHERE b = true;
SELECT * FROM tb WHERE f > 3;
SELECT * FROM tb WHERE s = 'hello';
```

## 9. IN-list and IN-subquery

```sql
SELECT * FROM t1 WHERE id IN (1, 3);                       -- literal list, coerced to col type
SELECT * FROM t1 WHERE id IN (SELECT t1_id FROM t2);       -- uncorrelated subquery
```

## 10. Scalar and EXISTS subqueries

```sql
SELECT * FROM t1 WHERE age > (SELECT MIN(age) FROM t1);
SELECT * FROM t1 WHERE age > (SELECT AVG(age) FROM t1);   -- F64 result into INT context
SELECT * FROM t1 WHERE EXISTS (SELECT 1 FROM t2 WHERE t2.t1_id = 2);
SELECT * FROM t1 WHERE NOT EXISTS (SELECT 1 FROM t2 WHERE v = 99999);
```

## 11. Transactions

```sql
BEGIN;
INSERT INTO t2 VALUES (9, 1, 900);
SELECT * FROM t2;
ROLLBACK;
SELECT * FROM t2;    -- row 9 gone

BEGIN;
INSERT INTO t2 VALUES (10, 1, 1000);
COMMIT;
SELECT * FROM t2;    -- row 10 persists

BEGIN TRANSACTION ISOLATION LEVEL READ COMMITTED;
SELECT * FROM t2;
COMMIT;

BEGIN TRANSACTION ISOLATION LEVEL SERIALIZABLE;
SELECT * FROM t2;
COMMIT;
```

## 12. Persistence after restart

Run against a populated database, then restart the server without deleting
`abdb_data/`, and verify the data is still there:

```bash
pkill -9 -f target/debug/abdb
nohup ./target/debug/abdb > /tmp/abdb.log 2>&1 &
```

```sql
SHOW TABLES;
SELECT * FROM t1;
SELECT * FROM t2;
```

## 13. Foreign keys

```sql
CREATE TABLE par (id INT PRIMARY KEY, name VARCHAR);
CREATE TABLE chi  (id INT PRIMARY KEY, pid INT, FOREIGN KEY (pid) REFERENCES par(id) ON DELETE CASCADE);
CREATE TABLE chi2 (id INT PRIMARY KEY, pid INT, FOREIGN KEY (pid) REFERENCES par(id) ON DELETE SET NULL);

INSERT INTO par VALUES (1, 'p1');
INSERT INTO par VALUES (2, 'p2');

INSERT INTO chi  VALUES (10, 1);                  -- ok
INSERT INTO chi  VALUES (11, 2);                  -- ok
INSERT INTO chi  VALUES (99, 99);                 -- err: dangling FK
INSERT INTO chi2 VALUES (20, 1);
INSERT INTO chi2 VALUES (21, 2);

DELETE FROM par WHERE id = 1;                     -- cascade deletes chi(10,1); nulls chi2(20,*)
SELECT * FROM chi;                                -- {(11,2)}
SELECT * FROM chi2;                               -- {(20,NULL),(21,2)}
SELECT * FROM par;                                -- {(2,p2)}
```

---

## Known limitations (still unresolved)

- **Correlated subqueries**: the binder has no outer‑scope resolution, so
  `EXISTS (SELECT 1 FROM c WHERE c.x = outer.x)` still returns a bind error.
- **Foreign keys are not persisted** across restarts (in‑memory catalog cache
  only). FK violations stop being enforced after a server restart until the
  tables are recreated.
- **UPDATE does not enforce FK constraints** (only INSERT and DELETE do).
- **DDL inside a user transaction** does not participate in rollback reliably;
  treat DDL as auto‑committing for now.
- **Subqueries are materialised before planning**, so very large subquery
  results inflate memory (no streaming, consistent with the rest of the
  executor's materialised model).
