# Changelog

All notable changes to this project are documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/).

Each release needs a `## [X.Y.Z]` section before its tag is pushed: the
release workflow publishes that section as the GitHub release notes.

## [Unreleased]

## [0.1.0-alpha.2] - 2026-11-09

Adds schema management: creating and altering tables, columns, indexes, foreign
keys and views from Tabularis's dialogs. Tested against CockroachDB v24.3 and
the latest release.

### Added

- **Schema management**: `get_create_table_sql`, `get_add_column_sql`,
  `get_alter_column_sql`, `get_create_index_sql`, `get_create_foreign_key_sql`,
  `drop_index`, `drop_foreign_key`, `create_view`, `alter_view` and `drop_view`.
  - Primary keys are named `<table>_pkey`, composite keys work, and `SERIAL`
    columns keep their width (`SMALLSERIAL`, `SERIAL4`, `SERIAL`).
  - A new primary key column is added and then extends the table's key through
    `ALTER PRIMARY KEY` (CockroachDB rejects `ADD COLUMN .. PRIMARY KEY` on a
    table that already has a key). A table with no key still generates, with a
    warning comment.
  - Altering a column emits one statement per change (rename, type, nullability,
    default, description). A change that converts data gets `USING` and a note
    about the session setting CockroachDB v24.3 needs.
  - Indexes are dropped as `schema.table@index`, so an index name shared by two
    tables is never ambiguous.
  - A view whose columns change is replaced by dropping and creating it in one
    transaction, so a failure in between restores the old view. A view's query
    resolves unqualified table names in the view's own schema.
  - Types, default expressions and view queries are validated before they are
    used in SQL (no `;`, no comment, nothing left open), names are always quoted,
    foreign key actions are limited to the five SQL actions, and drops never
    cascade.

### Changed

- `docs/architecture/04-ddl.md` records the host's real request and reply shapes
  and what was verified on v24.3 and v26.3.

### Known limitations

Tracked in [#16](https://github.com/TabularisDB/tabularis-cockroachdb-plugin/issues/16):

- `EXPLAIN` is not implemented; the explain button returns "Method not found".
- Materialized views, sequences, routines, triggers, multi-region metadata and
  the schema snapshot (ER diagram) are not provided yet.
- Schema management follows what the host's dialogs send. Options they do not
  offer are not generated: `STORING`, hash-sharded, inverted and partial indexes,
  `NOT VALID` foreign keys, comments on tables and schemas, and `LOCALITY`.
- On v24.3, a type change that converts data needs
  `SET enable_experimental_alter_column_type_general = true` in the same session.
  The host runs each generated statement separately, so the setting is explained
  in a comment and not emitted; v26.3 needs nothing.
- Replacing a view whose columns change makes it invisible for about 70 ms. A
  view other views depend on cannot be replaced, and an index behind a referenced
  `UNIQUE` constraint is not dropped (the error gives the `CASCADE` statement).
- Not exercised against a real CockroachDB Cloud cluster, nor query cancellation
  behind a load balancer; a real `40001` retry of an implicit statement could not
  be provoked on the tested releases.

## [0.1.0-alpha.1] - 2026-10-27

First public alpha. Connecting, browsing, querying and row editing work;
DDL generation and `EXPLAIN` are not implemented yet (see "Known limitations").
Tested against CockroachDB v24.3 and the latest release.

### Added

- **Connection and TLS**: pooled connections with keepalives and a 10 s connect
  timeout; TLS modes `disable`, `prefer`, `require`, `verify-ca` (hostname
  mismatch tolerated) and `verify-full`; custom CA and client certificates
  (PKCS#8, PKCS#1 and SEC1 keys); `cockroachdb://`, `cockroach://`,
  `postgresql://` and `postgres://` connection strings, including
  `options=--cluster=...` for CockroachDB Cloud. The server version is read
  from `version()`, other servers are refused with a pointer to the PostgreSQL
  driver, and common connection errors are explained.
- **Schema browsing**: databases, schemas, tables (comments, row estimates),
  views, columns (canonical types, comments, primary keys, hidden columns
  excluded), indexes (`STORING`, expression and hash-sharded columns handled),
  composite foreign keys, and `SHOW CREATE TABLE` DDL.
- **Result decoding**: exact `INT8` (a string beyond 2^53), `DECIMAL`, `JSONB`,
  `BYTES`, dates, times, intervals, UUID, `INET`, `BIT`, arrays, enums, spatial
  values, `VECTOR`, `TSVECTOR` and `TSQUERY`.
- **Query execution**: `execute_query` and `execute_query_batch` with
  pagination that understands comments, strings and `LIMIT`/`OFFSET`;
  transactions that span several runs of an editor tab (`session_id`,
  `release_session`); query cancellation; automatic retry of implicit
  transactions on `40001`; the transaction state is read from the server.
  Errors carry the SQLSTATE, `DETAIL` and `HINT`. A host display limit never
  changes what a statement writes, and session settings never leak to the next
  tab.
- **Row editing**: `insert_record`, `update_record` and `delete_record`, with
  type-aware binding, composite and `NULL` keys, the hidden `rowid` for tables
  without a primary key, and `BEGIN READ WRITE` so edits work on read-only
  default connections. A key that would change more than one row is refused,
  an empty key is refused, and editing a vanished row is a conflict.
- **BLOBs**: `fetch_blob_as_data_url`, `save_blob_to_file`, and upload of a
  picked file (`BLOB_FILE_REF`) within the host's `maxBlobSize`.
- Plugin settings `poolMaxSize`, `retryMaxAttempts` and `applicationName`.
- `.tabularium` manifest with CockroachDB metadata, icon, capabilities, data
  types, type mappings and plugin settings.
- CI (tests, clippy, rustfmt, manifest validation, markdownlint, cargo-audit,
  live CockroachDB tests on the oldest LTS line and the latest release),
  release and version-suggestion workflows, and the architecture documents in
  `docs/architecture`.

### Known limitations

Tracked in [#16](https://github.com/TabularisDB/tabularis-cockroachdb-plugin/issues/16):

- `EXPLAIN` is not implemented; the explain button returns "Method not found".
- DDL helpers (`get_create_table_sql`, `get_add_column_sql`,
  `get_alter_column_sql`, `get_create_index_sql`,
  `get_create_foreign_key_sql`, `drop_index`, `drop_foreign_key`) and view
  mutations are not implemented. The manifest advertises them, so the UI
  offers them and they fail. Running DDL as SQL in the editor works.
- Materialized views, sequences, routines, triggers, multi-region metadata and
  the schema snapshot (ER diagram) are not provided yet.
- Not exercised against a real CockroachDB Cloud cluster, nor query
  cancellation behind a load balancer; a real `40001` retry of an implicit
  statement could not be provoked on the tested releases.
