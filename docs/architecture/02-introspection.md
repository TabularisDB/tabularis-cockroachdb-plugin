# 02 · Introspection

General rules for every catalog query:

- **Qualify with the requested schema.** Every object is addressed as
  `<schema>.<name>`, two parts, never with a database part. Do not substitute
  `public` when the host sent a schema: objects outside the current
  `search_path` must still resolve.
- **Bind, don't interpolate.** Use `$1`, `$2` for names in `WHERE` clauses.
  `SHOW ...` statements cannot take bind parameters, so their identifiers go
  through `quote_identifier` (double the `"`).
- **Prefer `information_schema` and `SHOW` over `pg_catalog`.** CockroachDB's
  `pg_catalog` is an emulation built for client compatibility. It works for
  joins on OIDs, but several columns are always empty, and constraint
  details are not always reliable. Each query below names the source that is.
- **System objects**: hide database `system`. Hide schemas
  `crdb_internal`, `information_schema`, `pg_catalog` and `pg_extension`.
  Do **not** hide `defaultdb` or `postgres`: they are ordinary databases that
  users put tables in.

## `get_databases`

```sql
SELECT database_name FROM [SHOW DATABASES]
WHERE database_name <> 'system'
ORDER BY database_name
```

`SHOW DATABASES` also returns `owner`, `primary_region`, `secondary_region`,
`regions` and `survival_goal`. Keep them for the multi-region metadata in
phase 7. `pg_database` works as a fallback (`WHERE datistemplate = false`).

Database size: not available cheaply. `pg_database_size()` does not exist,
and summing `SHOW RANGES` is too expensive for a sidebar. Return no size.

## `get_schemas`

```sql
SELECT schema_name
FROM information_schema.schemata
WHERE schema_name NOT IN ('crdb_internal', 'information_schema', 'pg_catalog', 'pg_extension')
  AND schema_name NOT LIKE 'pg!_temp!_%' ESCAPE '!'
ORDER BY schema_name
```

The default schema is `public`. Owner and comment, when needed:

```sql
SELECT pg_catalog.pg_get_userbyid(n.nspowner),
       pg_catalog.obj_description(n.oid, 'pg_namespace')
FROM pg_catalog.pg_namespace n
WHERE n.nspname = $1
```

Schema grants use `aclexplode`, which exists from v22.2. Before that, return
an empty grant list instead of failing. `SHOW GRANTS ON SCHEMA <schema>` is
an alternative that works on every supported release.

## `get_tables`

```sql
SELECT table_name, type, estimated_row_count, locality
FROM [SHOW TABLES FROM <database>.<schema>]
WHERE type = 'table'
ORDER BY table_name
```

- `SHOW TABLES` is the only cheap source of a row estimate
  (`estimated_row_count`, from table statistics) and of the table's
  multi-region `locality` (`REGIONAL BY TABLE IN PRIMARY REGION`,
  `REGIONAL BY ROW`, `GLOBAL`). Return the estimate wherever the host asks for
  an approximate count.
- `type` is `table`, `view`, `materialized view` or `sequence`. Filter here
  and reuse the same query for views, materialized views and sequences.
  Verify the exact `type` strings on the CI matrix.
- Table comments, batched for the whole schema:

```sql
SELECT c.relname, d.description
FROM pg_catalog.pg_class c
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
JOIN pg_catalog.pg_description d ON d.objoid = c.oid AND d.objsubid = 0
WHERE n.nspname = $1
```

Fallback when `SHOW TABLES` is unavailable to the user's role:
`information_schema.tables` with `table_type` (`BASE TABLE`, `VIEW`,
`MATERIALIZED VIEW`, `SEQUENCE`). Verify the exact `table_type` strings on the
CI matrix.

## `get_columns`

```sql
SELECT c.column_name,
       c.crdb_sql_type,
       c.data_type,
       c.udt_schema,
       c.udt_name,
       c.is_nullable,
       c.column_default,
       c.is_hidden,
       c.is_identity,
       c.identity_generation,
       c.is_generated,
       c.generation_expression,
       c.collation_name,
       c.character_maximum_length,
       c.numeric_precision,
       c.numeric_scale,
       c.ordinal_position,
       d.description,
       (pk.column_name IS NOT NULL) AS is_pk
FROM information_schema.columns c
LEFT JOIN pg_catalog.pg_namespace n ON n.nspname = c.table_schema
LEFT JOIN pg_catalog.pg_class cls ON cls.relname = c.table_name AND cls.relnamespace = n.oid
LEFT JOIN pg_catalog.pg_description d ON d.objoid = cls.oid AND d.objsubid = c.ordinal_position
LEFT JOIN (
    SELECT kcu.column_name
    FROM information_schema.table_constraints tc
    JOIN information_schema.key_column_usage kcu
      ON tc.constraint_name = kcu.constraint_name
     AND tc.table_schema = kcu.table_schema
     AND tc.table_name = kcu.table_name
    WHERE tc.constraint_type = 'PRIMARY KEY'
      AND tc.table_schema = $1
      AND tc.table_name = $2
) pk ON pk.column_name = c.column_name
WHERE c.table_schema = $1 AND c.table_name = $2
ORDER BY c.ordinal_position
```

Points that matter:

- **Join constraints on table name too.** Every table's primary key
  constraint is called `<table>_pkey` (or `primary` on clusters created
  before v22), and constraint names are only unique per table. Joining
  `table_constraints` to `key_column_usage` on name and schema alone pairs
  every table's primary key with every other table's, and marks wrong
  columns as primary keys. The `tc.table_name = kcu.table_name` predicate is
  required, and needs a regression test.
- **Hidden columns.** `is_hidden = 'YES'` marks `rowid` (added when a table
  has no explicit primary key), `crdb_region` on `REGIONAL BY ROW` tables and
  `crdb_internal_*_shard_*` columns of hash-sharded indexes. Leave them out of
  `get_columns` by default. Keep `rowid` available to CRUD: a table without a
  declared primary key is keyed by it (see [07](./07-crud-and-export.md)).
- **Type names.** `crdb_sql_type` is CockroachDB's canonical spelling
  (`INT8`, `STRING`, `JSONB`, `STRING[]`, `DECIMAL(10,2)`). Show that.
  `data_type` is the PostgreSQL spelling (`bigint`, `text`). Keep it for type
  families. `data_type = 'USER-DEFINED'` with `udt_name` is an enum or another
  user-defined type, see [03](./03-types.md).
- **Defaults.** `unique_rowid()` and `gen_random_uuid()` defaults mark
  auto-generated keys. `nextval('seq'::REGCLASS)` marks a sequence.
- **Generated and identity columns** come from `is_generated`,
  `generation_expression`, `is_identity` and `identity_generation`.

`get_all_columns_batch` and `get_schema_snapshot` use the same query without
the table predicate. Add `c.table_name` to the select list and to the
`pk` join, and order by `c.table_name, c.ordinal_position`. Implement the
batch form: a per-table N+1 loop is slow on a cluster with many tables.

## `get_indexes`

```sql
SHOW INDEXES FROM <schema>.<table>
```

Columns: `table_name`, `index_name`, `non_unique`, `seq_in_index`,
`column_name`, `definition`, `direction`, `storing`, `implicit`, `visible`.

- Group rows by `index_name`, keep `seq_in_index` order.
- `implicit = true`: a column CockroachDB added to the key (the primary key
  suffix of secondary indexes, the shard column, `crdb_region`). Skip it.
- `storing = true`: a `STORING` (covering) column. Not part of the key.
  Report it separately as an included column, not in the column list.
- `definition` holds the expression for expression indexes.
- `visible = false`: a `NOT VISIBLE` index. Report it.
- Primary key: `index_name` is the primary key constraint name. Match on the
  `<table>_pkey` suffix or `primary`. Better: compare with
  `tc.constraint_name` from the primary key lookup.
- Index kind (inverted, hash-sharded, partial, vector) and the partial `WHERE`
  are not in `SHOW INDEXES`. Read them from `pg_indexes.indexdef` or from
  `SHOW CREATE TABLE` when the UI needs them.
- Index names are scoped to the table: two tables in one schema can both have
  `idx_name`. Never look indexes up by name alone.

Batch form for `get_schema_snapshot`: `SHOW INDEXES FROM DATABASE <database>`
(verify on the CI matrix), or `pg_indexes` filtered by schema.

## `get_foreign_keys`

```sql
SELECT c.conname,
       ref_ns.nspname  AS ref_schema,
       ref_cl.relname  AS ref_table,
       c.confupdtype,
       c.confdeltype,
       k.ord,
       a.attname       AS column_name,
       ra.attname      AS ref_column_name
FROM pg_catalog.pg_constraint c
JOIN pg_catalog.pg_class cl       ON cl.oid = c.conrelid
JOIN pg_catalog.pg_namespace ns   ON ns.oid = cl.relnamespace
JOIN pg_catalog.pg_class ref_cl   ON ref_cl.oid = c.confrelid
JOIN pg_catalog.pg_namespace ref_ns ON ref_ns.oid = ref_cl.relnamespace
CROSS JOIN LATERAL unnest(c.conkey, c.confkey) WITH ORDINALITY AS k(col, ref_col, ord)
JOIN pg_catalog.pg_attribute a  ON a.attrelid = c.conrelid  AND a.attnum = k.col
JOIN pg_catalog.pg_attribute ra ON ra.attrelid = c.confrelid AND ra.attnum = k.ref_col
WHERE c.contype = 'f' AND ns.nspname = $1 AND cl.relname = $2
ORDER BY c.conname, k.ord
```

- **Composite keys pair by position.** Column `i` of `conkey` references column
  `i` of `confkey`. Joining both attribute lists independently produces a
  cross product and wrong pairs. Cover a two-column FK with a test.
- Multi-argument `unnest ... WITH ORDINALITY` and `LATERAL` support must be
  verified on the oldest CI release. If either is missing, read
  `conkey`/`confkey` as arrays and pair them in Rust.
- Actions: `a` NO ACTION, `r` RESTRICT, `c` CASCADE, `n` SET NULL,
  `d` SET DEFAULT.
- Alternative source: `information_schema.referential_constraints` joined
  with `key_column_usage` on `position_in_unique_constraint`. Use it if the
  `pg_constraint` arrays turn out to be unreliable on a release.

Implement `get_all_foreign_keys_batch` with the same query without the table
predicate.

## Check constraints

```sql
SELECT con.conname, pg_get_constraintdef(con.oid), con.convalidated
FROM pg_catalog.pg_constraint con
JOIN pg_catalog.pg_class cls ON cls.oid = con.conrelid
JOIN pg_catalog.pg_namespace ns ON ns.oid = cls.relnamespace
WHERE con.contype = 'c' AND ns.nspname = $1 AND cls.relname = $2
ORDER BY con.conname
```

Hash-sharded indexes add a hidden check constraint on the shard column. Leave
out constraints whose only column is hidden.

## `get_table_ddl`

```sql
SHOW CREATE TABLE <schema>.<table>
```

Read the `create_statement` column. The output already includes indexes,
foreign keys, families, localities and comments as CockroachDB will accept
them back. Return it unchanged.

## Views and materialized views

- `get_views`: `SHOW TABLES` filtered on `type = 'view'`.
- `get_materialized_views`: `type = 'materialized view'`.
- `get_view_definition` / `get_materialized_view_definition`:
  `SHOW CREATE VIEW <schema>.<view>`, column `create_statement`. It works for
  both kinds.
- `get_view_columns`: the `get_columns` query; views have no primary key.
- `refresh_materialized_view`: `REFRESH MATERIALIZED VIEW <schema>.<view>`.

## Sequences

```sql
SELECT sequence_name, data_type, start_value, minimum_value, maximum_value, increment
FROM information_schema.sequences
WHERE sequence_schema = $1
ORDER BY sequence_name
```

Current value, on demand only: `SELECT last_value FROM <schema>.<sequence>`.
`SHOW CREATE SEQUENCE` returns the DDL.

## Routines

Gated on v22.2 (functions) and v23.2 (procedures).

```sql
SELECT r.routine_name, r.routine_type, r.data_type, r.specific_name
FROM information_schema.routines r
WHERE r.routine_schema = $1
ORDER BY r.routine_name
```

- Parameters: `information_schema.parameters` by `specific_name`, ordered by
  `ordinal_position`.
- Definition: `SHOW CREATE FUNCTION <schema>.<name>` or
  `SHOW CREATE PROCEDURE <schema>.<name>`, column `create_statement`. It
  returns every overload, so split by `specific_name` when there are several.
- Built-in functions also show up in `pg_proc`. Filter by schema, never list
  `pg_catalog` routines.

## Enums and user-defined types

```sql
SELECT schema, name, values FROM [SHOW ENUMS FROM <database>.<schema>]
```

Or `pg_type` joined with `pg_enum`, ordered by `enumsortorder`. The values
feed the cell editor's dropdown and the binding casts in
[03](./03-types.md).

## Multi-region metadata (phase 7)

- Database: `SHOW REGIONS FROM DATABASE <database>` and
  `SHOW SURVIVAL GOAL FROM DATABASE <database>`.
- Table: `locality` from `SHOW TABLES` (above).
- Show them in table metadata. They affect DDL generation
  (`LOCALITY REGIONAL BY ROW` in `CREATE TABLE`), not query results.
