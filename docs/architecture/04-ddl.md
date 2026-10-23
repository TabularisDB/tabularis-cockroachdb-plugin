# 04 · DDL

The DDL generators return SQL text. The host shows it to the user and runs
it through `execute_query`. All of them are pure functions of their input;
unit-test them without a server.

## How schema changes behave

- **Online and asynchronous.** Most schema changes (adding a column with a
  default, creating an index, changing the primary key) run as background
  jobs that backfill without locking the table. In an implicit transaction,
  the statement returns when the job is done.
- **Not transactional in the PostgreSQL sense.** Inside an explicit
  transaction, schema changes only start running at `COMMIT`, a failure at
  that point can leave part of the transaction applied, and mixing DDL with
  writes to the same table in one transaction is restricted. Run each
  generated DDL statement on its own, never inside a wrapping `BEGIN`.
  `supportsTransactionalDDL`-style behavior is **false** for this driver.
- **Long-running.** Backfills on large tables take minutes. The host's plugin
  call timeout applies; `SHOW JOBS` shows progress after a timeout. The job
  keeps running after the client gives up.

## `get_create_table_sql`

```sql
CREATE TABLE <schema>.<table> (
    id UUID NOT NULL DEFAULT gen_random_uuid(),
    name STRING NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT <table>_pkey PRIMARY KEY (id)
)
```

- **Always declare a primary key.** Without one, CockroachDB adds a hidden
  `rowid INT8 DEFAULT unique_rowid()` column and uses it as the key. That
  works, but it is rarely what the user wants and it complicates row editing.
  If the user defines no primary key, generate the statement anyway and
  warn in a SQL comment.
- **Auto-increment.** The manifest declares `serial_type: "SERIAL"`. The
  server normalizes it to `INT8 DEFAULT unique_rowid()`: unique and roughly
  ordered, but not sequential and not dense. Offer `UUID DEFAULT
  gen_random_uuid()` as the alternative in the default-value menu. Avoid
  sequence-backed keys: a monotonically increasing key writes every insert to
  the same range, a hotspot in a distributed cluster.
- Default-value menu: `gen_random_uuid()`, `unique_rowid()`, `now()`,
  `current_timestamp()`, `current_date`, `true`, `false`.
- Name the primary key constraint `<table>_pkey`, as CockroachDB does.
- Column comments are separate statements after the `CREATE TABLE`
  (`COMMENT ON COLUMN`).
- Multi-region tables: append `LOCALITY REGIONAL BY ROW`, `LOCALITY GLOBAL`
  or `LOCALITY REGIONAL BY TABLE IN <region>` when requested (phase 7).

## `get_add_column_sql`

```sql
ALTER TABLE <schema>.<table> ADD COLUMN <column> <type> [NOT NULL] [DEFAULT <expr>]
```

`NOT NULL` without a `DEFAULT` fails on a table with rows, as in PostgreSQL.
`ADD COLUMN IF NOT EXISTS` is supported.

## `get_alter_column_sql`

One statement per change:

```sql
ALTER TABLE <schema>.<table> RENAME COLUMN <old> TO <new>
ALTER TABLE <schema>.<table> ALTER COLUMN <column> SET DEFAULT <expr>
ALTER TABLE <schema>.<table> ALTER COLUMN <column> DROP DEFAULT
ALTER TABLE <schema>.<table> ALTER COLUMN <column> SET NOT NULL
ALTER TABLE <schema>.<table> ALTER COLUMN <column> DROP NOT NULL
ALTER TABLE <schema>.<table> ALTER COLUMN <column> TYPE <type> [USING <expr>]
```

- **Type changes.** Changes that do not rewrite data (widening `VARCHAR(n)`,
  `INT4` to `INT8`) run in place. Changes that rewrite data need a
  backfill. On older releases they also need
  `SET enable_experimental_alter_column_type_general = true` in the session
  and cannot run inside an explicit transaction. Verify the gate per release
  on the CI matrix, and emit the `SET` before the `ALTER` only when the
  connected version needs it.
- A column that is part of the primary key or of an index cannot change type
  in place. Report the server error; do not try to rebuild the table.

## Primary keys (`alter_primary_key`)

```sql
ALTER TABLE <schema>.<table> ALTER PRIMARY KEY USING COLUMNS (<col>, ...)
```

- `ALTER PRIMARY KEY` keeps the old primary key's columns as a new unique
  secondary index, unless the old key was the hidden `rowid`. To replace the
  key without leaving that index, emit both changes in one statement:

```sql
ALTER TABLE <schema>.<table>
    DROP CONSTRAINT <table>_pkey,
    ADD CONSTRAINT <table>_pkey PRIMARY KEY (<col>, ...)
```

- Primary key columns must be `NOT NULL`. Emit `ALTER COLUMN ... SET NOT NULL`
  first when needed.
- Hash-sharded primary key, for sequential keys:
  `... PRIMARY KEY (<col>) USING HASH`.

## Indexes

```sql
CREATE [UNIQUE] INDEX [IF NOT EXISTS] <index> ON <schema>.<table> (<col> [ASC|DESC], ...)
    [USING HASH]
    [STORING (<col>, ...)]
    [WHERE <predicate>]
CREATE INVERTED INDEX <index> ON <schema>.<table> (<jsonb_or_array_col>)
CREATE VECTOR INDEX <index> ON <schema>.<table> (<vector_col>)   -- v25.2+
```

- `STORING` is CockroachDB's `INCLUDE`. Accept `INCLUDE` from the UI and emit
  `STORING`.
- `USING GIN` is accepted as a synonym for an inverted index.
- `CONCURRENTLY` is accepted and ignored: every index build is online.
- Expression indexes are supported (`(lower(email))`).

### `drop_index`

```sql
DROP INDEX <schema>.<table>@<index> [CASCADE]
```

Index names are unique per table, not per schema. `DROP INDEX
<schema>.<index>` is ambiguous when two tables share an index name, so always
use the `table@index` form. The same applies to renames:

```sql
ALTER INDEX <schema>.<table>@<index> RENAME TO <new>
```

A unique index that backs a `UNIQUE` constraint referenced by a foreign key
needs `CASCADE`.

## Foreign keys

```sql
ALTER TABLE <schema>.<table>
    ADD CONSTRAINT <fk> FOREIGN KEY (<col>, ...)
    REFERENCES <schema>.<ref_table> (<ref_col>, ...)
    [ON DELETE <action>] [ON UPDATE <action>]
    [NOT VALID]
```

- The referenced columns must be covered by a unique index or the primary
  key.
- `NOT VALID` skips validating existing rows; `ALTER TABLE ... VALIDATE
  CONSTRAINT <fk>` validates later.
- Cross-database foreign keys are rejected by the server by default. Do not
  offer tables from another database as targets.
- `drop_foreign_key`: `ALTER TABLE <schema>.<table> DROP CONSTRAINT <fk>`.

## Comments

```sql
COMMENT ON DATABASE <database> IS '<text>'
COMMENT ON SCHEMA <schema> IS '<text>'
COMMENT ON TABLE <schema>.<table> IS '<text>'
COMMENT ON COLUMN <schema>.<table>.<column> IS '<text>'
COMMENT ON INDEX <schema>.<table>@<index> IS '<text>'
COMMENT ON CONSTRAINT <constraint> ON <schema>.<table> IS '<text>'
```

`IS NULL` removes a comment. There is **no `COMMENT ON VIEW`** or
`COMMENT ON MATERIALIZED VIEW`: return no statement for those, and hide the
comment action for views.

## Renames and drops

```sql
ALTER DATABASE <database> RENAME TO <new>
ALTER SCHEMA <schema> RENAME TO <new>
ALTER TABLE <schema>.<table> RENAME TO <new>       -- bare new name, same schema
ALTER TABLE <schema>.<table> SET SCHEMA <other>    -- move between schemas
ALTER VIEW <schema>.<view> RENAME TO <new>
DROP TABLE [IF EXISTS] <schema>.<table> [CASCADE]
DROP VIEW [IF EXISTS] <schema>.<view> [CASCADE]
DROP MATERIALIZED VIEW [IF EXISTS] <schema>.<view> [CASCADE]
DROP SCHEMA [IF EXISTS] <schema> [CASCADE]
DROP DATABASE [IF EXISTS] <database> [CASCADE]
CREATE SCHEMA [IF NOT EXISTS] <schema> [AUTHORIZATION <role>]
CREATE DATABASE [IF NOT EXISTS] <database>
```

`DROP DATABASE` cannot target the current database of the connection. Run it
from a connection to another database (`defaultdb`).

## Views

```sql
CREATE VIEW <schema>.<view> AS <select>
CREATE OR REPLACE VIEW <schema>.<view> AS <select>
CREATE MATERIALIZED VIEW <schema>.<view> AS <select>
```

- `alter_view`: emit `CREATE OR REPLACE VIEW`. CockroachDB has no
  `ALTER VIEW ... AS`. `CREATE OR REPLACE` only works when the new column list
  starts with the old one, with the same types. Otherwise emit `DROP VIEW`
  followed by `CREATE VIEW`, and say so in the preview.
- Materialized views cannot be replaced: drop and create.

## Routines (phase 7)

- `routine_create_template`:

  ```sql
  CREATE OR REPLACE FUNCTION <schema>.<name>(arg INT8)
  RETURNS INT8
  LANGUAGE SQL
  AS $$
    SELECT arg + 1
  $$
  ```

  `LANGUAGE PLpgSQL` from v23.2. Procedures (`CREATE PROCEDURE`, `CALL`)
  from v23.2.

- `drop_routine`: `DROP FUNCTION <schema>.<name>(<arg types>)` with the
  argument types, to select one overload.
- `routine_management` stays `false` in the manifest until these are done.
