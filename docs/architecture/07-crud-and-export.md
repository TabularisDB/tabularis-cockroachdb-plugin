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

## `insert_record`

```sql
INSERT INTO <schema>.<table> (<col>, ...) VALUES ($1, ...) RETURNING *
```

- Columns the user left untouched are left out, so server defaults
  (`unique_rowid()`, `gen_random_uuid()`, `now()`) apply.
- `RETURNING *` gives the host the generated key and defaults in one round
  trip.
- `UPSERT INTO` exists and is cheaper than `INSERT ... ON CONFLICT` on the
  primary key. Use it only where the host explicitly asks for an upsert.

## `update_record`

```sql
UPDATE <schema>.<table> SET <col> = $1, ... WHERE <pk_col> = $n AND ... RETURNING *
```

Report 0 affected rows as a conflict ("row not found; it may have been
changed or deleted"), not as success.

## `delete_record`

```sql
DELETE FROM <schema>.<table> WHERE <pk_col> = $1 AND ...
```

## Saving several edits

When the host saves several changes together, run them in one driver-owned
transaction with the retry protocol from [05](./05-query-execution.md):

```sql
BEGIN READ WRITE;
SAVEPOINT cockroach_restart;
-- INSERT / UPDATE / DELETE statements
RELEASE SAVEPOINT cockroach_restart;
COMMIT;
```

`BEGIN READ WRITE` keeps saves working on connections whose default is
read-only. Check the session status first: if the user's editor session has
a transaction open, run the statements inside it and add no transaction
statements of your own.

## BLOBs

`save_blob_to_file` and `fetch_blob_as_data_url` work on `BYTES` columns.
Reuse the PostgreSQL plugin's `blob.rs`: the wire format is the same
(`bytea`, OID 17). Select the single cell by primary key, never the whole
row.

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
