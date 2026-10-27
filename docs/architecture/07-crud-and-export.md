# 07 · CRUD and export

## Row identity

`update_record` and `delete_record` identify a row by its primary key.

- Tables with a declared primary key: use its columns, all of them for
  composite keys.
- Tables without one: CockroachDB keys them by the hidden `rowid` column.
  `get_columns` hides it (see [02](./02-introspection.md)), so the data grid
  query has to select it explicitly for those tables
  (`SELECT rowid, * FROM ...`), and CRUD uses it as the key. If that is not
  possible in the grid, report the table as read-only instead of matching on
  every column.
- `INT8` keys travel as strings when they exceed 2^53 (see
  [03](./03-types.md)). Bind them back from the string, never through a
  float.

## Request and reply shape

The host's real contract, which differs from the plugin guide:

| Method | Request | Reply |
| --- | --- | --- |
| `insert_record` | `table`, `data` (column -> value), `schema`, `max_blob_size` | number of affected rows |
| `update_record` | `table`, `pk_map`, `col_name`, `new_val`, `schema`, `max_blob_size` | number of affected rows |
| `delete_record` | `table`, `pk_map`, `schema` | number of affected rows |

`pk_map` maps every key column to its value, so composite keys work. The reply
is a number: the host rejects `null`, which is why the statements do not use
`RETURNING *` (the host has nowhere to put the returned row).

Column types come from `information_schema.columns` (including hidden columns,
so `rowid` can be a key) and drive `binding::bind_value`. A column named in a
request must exist in the table, and an unknown table is reported as such.

## `insert_record`

```sql
INSERT INTO <schema>.<table> (<col>, ...) VALUES ($1, ...)
INSERT INTO <schema>.<table> DEFAULT VALUES   -- no columns given
```

Columns the user left untouched are left out, so server defaults
(`unique_rowid()`, `gen_random_uuid()`, `now()`) apply.
`UPSERT INTO` exists and is cheaper than `INSERT ... ON CONFLICT` on the
primary key. Use it only where the host explicitly asks for an upsert.

## `update_record`

```sql
UPDATE <schema>.<table> SET <col> = $1 WHERE <pk_col> = $n AND ...
```

- A `NULL` key value is matched with `IS NULL`.
- The value `__USE_DEFAULT__` sets the column to `DEFAULT`. It means that only
  for a value being set, never for a key.
- 0 affected rows is reported as a conflict ("row not found; it may have been
  changed or deleted"), not as success.

## `delete_record`

```sql
DELETE FROM <schema>.<table> WHERE <pk_col> = $1 AND ...
```

0 affected rows is a normal reply (the row is already gone).

## Safety rules

- An empty or missing `pk_map` is refused for `update_record`, `delete_record`
  and the BLOB methods, so a request can never match every row.
- An update or delete that would change **more than one row** is rolled back
  and refused ("the row key matches more than one row"). A key that does not
  identify a single row (a table without a primary key whose rows repeat) is
  never allowed to change several rows silently.

## Transactions and retries

Each write is its own driver-owned transaction:

```sql
BEGIN READ WRITE;
-- the statement
COMMIT;
```

`BEGIN READ WRITE` keeps edits working on a connection or role whose default is
read-only (`default_transaction_read_only = on`): a plain write fails there with
`SQLSTATE 25006`. On `40001`, at the statement or at `COMMIT`, the transaction
is rolled back and run again, up to `retryMaxAttempts`, with the backoff from
[05](./05-query-execution.md). Restarting the whole transaction is equivalent to
the `SAVEPOINT cockroach_restart` protocol for a transaction of one statement,
and also covers a failure at `COMMIT`, which a savepoint cannot.

The host sends no `session_id` with these calls and has no batch method, so
there is never an open user transaction to join and no multi-statement save to
wrap. If the host gains either, run the statements in one transaction with the
savepoint protocol:

```sql
BEGIN READ WRITE;
SAVEPOINT cockroach_restart;
-- INSERT / UPDATE / DELETE statements
RELEASE SAVEPOINT cockroach_restart;
COMMIT;
```

A connection whose cleanup fails after an error is closed, not returned to the
pool.

## BLOBs

`save_blob_to_file` and `fetch_blob_as_data_url` work on `BYTES` columns
(`bytea`, OID 17, the same wire format as the PostgreSQL plugin). They read the
single cell chosen by `pk_map`, never the whole row, and name the problem when
the row is missing, the cell is `NULL`, or the column is not `BYTES`.

- Preview and writes use `BLOB:<size>:<mime>:<base64>`.
- A file the user picks arrives as `BLOB_FILE_REF:<size>:<mime>:<path>`. It is
  read and stored as bytes, up to the host's `max_blob_size` (default 100 MiB).
  An unreadable or oversized file is an error; its path is never stored as
  text.

## Literals in generated SQL

The host generates SQL (filters, Copy as INSERT, dumps) from the manifest's
`sql_dialect: "postgres"`. These rules hold on CockroachDB:

- Strings: `'...'` with `'` doubled. `standard_conforming_strings` is always
  `on`, so a backslash is an ordinary character in `'...'`. `E'...'` with
  backslash escapes also works.
- Identifiers: `"..."` with `"` doubled.
- Booleans: `TRUE` / `FALSE`.
- Bytes: `decode('<hex>', 'hex')` is the most portable form for dumps.
  `'\x<hex>'::BYTES` also works.
- `INT8` above 2^53: write the digits, never a float.
- `DECIMAL`: write the exact digits.
- Arrays: `ARRAY[...]` or `'{...}'::<type>[]`.
- Dollar quoting (`$$ ... $$`) is supported, so the postgres preset's lexer
  is correct for routine bodies.

## SQL dump prologue

A dump for CockroachDB starts with:

```sql
SET client_encoding = 'UTF8';
```

There is no epilogue. Do not emit PostgreSQL-only prologue statements
(`SET session_replication_role`, `SELECT pg_catalog.set_config(...)`,
`SET default_tablespace`): they fail or are ignored. A dump of table DDL
should come from `SHOW CREATE TABLE`, which already contains indexes and
foreign keys. Do not add a second `ALTER TABLE ... ADD CONSTRAINT` pass for
foreign keys; that fails on duplicate constraint names.

For large exports, `EXPORT INTO CSV` and `BACKUP` run on the cluster and
write to cloud storage. They are not client-side exports, so the driver does
not use them.

## Copying objects between engines

Index names are unique per table on CockroachDB, but unique per schema on
PostgreSQL and SQLite. When the host copies tables from CockroachDB to one of
those engines, it has to prefix index names with the table name to avoid
collisions. Copying into CockroachDB keeps names unchanged.
