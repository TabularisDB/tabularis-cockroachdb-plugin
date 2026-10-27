# Architecture

These documents specify what the CockroachDB driver has to do, area by area,
and the CockroachDB behavior each part has to account for. They are the
implementation plan for turning the `@tabularis/create-plugin` scaffold into a
complete driver.

## Why a dedicated driver

CockroachDB speaks the PostgreSQL wire protocol, so a PostgreSQL client can
connect and run simple queries. A PostgreSQL driver still gets a lot wrong:

- `pg_catalog` is an emulation. Some columns are always empty, some functions
  (`pg_database_size`, `pg_size_pretty`, `pg_postmaster_start_time`) do not
  exist, and ACL functions only exist on recent releases.
- The server reports PostgreSQL version `13.0.0` whatever its real release is,
  so `server_version_num` cannot be used to gate features.
- Index names are scoped to their table, not to the schema.
- Tables without a primary key get a hidden `rowid` column.
- `INT` means `INT8`, `SERIAL` means `INT8 DEFAULT unique_rowid()`, and
  `unique_rowid()` values exceed JavaScript's safe-integer range.
- Every transaction is `SERIALIZABLE` by default, and the client is expected
  to retry on `SQLSTATE 40001`.
- `EXPLAIN` returns an indented text tree, not PostgreSQL's format, and
  `EXPLAIN (FORMAT JSON)` is not supported.
- Schema changes are online, asynchronous jobs with their own rules inside
  explicit transactions.

## Documents

| # | Document | Covers |
| --- | --- | --- |
| 01 | [Connection](./01-connection.md) | Parameters, URIs, TLS, cluster routing, pooling, bootstrap, version detection |
| 02 | [Introspection](./02-introspection.md) | Databases, schemas, tables, columns, keys, indexes, views, sequences, routines, comments |
| 03 | [Types](./03-types.md) | Type names, result decoding, JSON encoding, parameter binding |
| 04 | [DDL](./04-ddl.md) | DDL generators, primary keys, indexes, comments, renames, drops |
| 05 | [Query execution](./05-query-execution.md) | Transactions, retries, sessions, cancellation, pagination, autocommit-only statements |
| 06 | [EXPLAIN](./06-explain.md) | Plan variants and parsing the text tree into Tabularis's plan shape |
| 07 | [CRUD and export](./07-crud-and-export.md) | Row editing, BLOBs, literals and SQL dumps |
| 08 | [Capabilities](./08-capabilities.md) | Manifest flags, the RPC methods behind each one, plugin settings |
| 09 | [Testing](./09-testing.md) | Unit tests, live-cluster tests, CI matrix |

## Implementation phases

Each phase ends with a release on the `alpha` channel. A capability flag in
`.tabularium` already declares the target behavior, so a flag whose handlers
are not done yet shows errors in the UI. That is expected during the alpha.

| Phase | Scope | Documents | Status |
| --- | --- | --- | --- |
| 1. Connect | Pool, TLS, URI parsing, `test_connection`, version detection | 01 | Done |
| 2. Browse | Databases, schemas, tables, columns, PKs, FKs, indexes, views, DDL text | 02, 03 | Done |
| 3. Query | `execute_query`, batches, sessions, cancellation, retries | 05, 03 | Done |
| 4. Edit | `insert_record`, `update_record`, `delete_record`, BLOBs | 07, 03 | Done |
| 5. DDL | Create/alter table, column, index, FK, comments, drops | 04 | To do |
| 6. Plans | `explain_query` with the text-tree parser | 06 | To do |
| 7. Extras | Materialized views, sequences, routines, multi-region metadata, schema snapshot | 02, 04 | To do |
| 8. Release | Live tests on the CI matrix, screenshots, `1.0.0` | 09 | In progress (`0.1.0-alpha.1`) |

Phases 5 and 6 are tracked in issue #16.

## Starting point

[`tabularis-postgresql-plugin`](https://github.com/TabularisDB/tabularis-postgresql-plugin)
is the closest existing driver. Reuse its structure and crates rather than
starting from the scaffold's stubs:

- `tokio-postgres` + `deadpool-postgres` for connections and pooling
- `tokio-postgres-rustls` + `rustls-platform-verifier` for TLS
- its module layout: `binding.rs` (value binding cascade), `extract.rs`
  (result decoding), `session.rs` (connections pinned to an editor session),
  `cancel.rs`, `settings.rs`
- its RPC surface: `execute_query_batch`, `release_session`, `cancel`,
  `get_table_ddl`, `shutdown`, on top of the scaffold's methods

Copy code, do not depend on that crate: every module needs CockroachDB-specific
changes, described in the documents above.

## Conventions used in these documents

- **SQL is quoted verbatim.** `$1`, `$2` are bind parameters. `<schema>` and
  `<table>` mark values that must be quoted with `quote_identifier` or
  `quote_literal` when they cannot be bound.
- **"Verify on the CI matrix"** marks behavior that changed between
  CockroachDB releases or that these documents could not pin to a version.
  Confirm it with a live test before relying on it (see [09](./09-testing.md)).
- **Supported versions**: the oldest supported line is the oldest
  CockroachDB LTS release still in support (currently v24.3). The CI matrix
  runs it and `latest`.
