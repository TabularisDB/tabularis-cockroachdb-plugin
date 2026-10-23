# 09 · Testing

## Unit tests (no server)

Each pure function sits next to a sibling `_tests.rs` file and runs in
`cargo test --lib --bins`.

| Area | What to assert |
| --- | --- |
| URI parsing | All four schemes; case-insensitive scheme; empty path gives `defaultdb`; missing port gives `26257`; `26258` is kept; empty credentials; `options=--cluster%3Dx` is decoded once; `sslrootcert` overrides the form |
| Version parsing | `CockroachDB CCL v24.3.4 (...)`, `CockroachDB OSS v23.1.0`, pre-release tags (`v25.2.0-beta.1`), a vanilla PostgreSQL `version()` returns `None` |
| Identifier quoting | Embedded `"` is doubled; mixed case kept; every catalog statement has exactly two name parts, with no database part |
| Catalog SQL | The requested schema is used, never replaced by `public`; the primary key join includes `tc.table_name = kcu.table_name`; the batch form adds `c.table_name` |
| Index grouping | `implicit` rows dropped; `storing` columns kept apart from key columns; `seq_in_index` order; primary key detection |
| FK pairing | A two-column FK pairs columns by position |
| Binding cascade | Each step, and the order: enum before text, `DECIMAL` before numbers, `INT8` from string |
| Result encoding | `INT8` at 2^53 − 1 is a number, 2^53 and `unique_rowid()`-sized values are strings; `DECIMAL` is a string; `JSONB` is a JSON value |
| DDL generators | `table@index` in `DROP INDEX` and index renames; `ALTER PRIMARY KEY`; no `COMMENT ON VIEW`; `STORING` instead of `INCLUDE`; `CREATE OR REPLACE VIEW` for `alter_view` |
| Retry classifier | `40001` retried only outside a user transaction and before streaming; `40003` never retried |
| Autocommit classifier | `SET CLUSTER SETTING`, `BACKUP`, `RESTORE`, `IMPORT` outside the wrap; `WITH detached` inside; leading comments; lower case |
| EXPLAIN parser | The fixtures listed in [06](./06-explain.md) |
| TLS verifier | `verify-ca` accepts a valid chain with a mismatched hostname; `verify-full` rejects it (locally generated CA, no server) |

## Live-cluster tests

`tests/live_db.rs` drives the compiled binary over stdio, the way the host
does, against a real node. CI runs it in the `live-db-integration` job once
the file exists:

| Variable | CI value |
| --- | --- |
| `COCKROACHDB_PLUGIN_BIN` | `target/debug/cockroachdb-plugin` |
| `CRDB_HOST` / `CRDB_PORT` | `127.0.0.1` / `26257` |
| `CRDB_USER` | `root` |
| `CRDB_DATABASE` | `testdb` |
| `CRDB_SSLMODE` | `disable` (the CI node runs `--insecure`) |

The tests create their own schema per run (`test_<random>`) and drop it at
the end. Cover at least:

- `test_connection` returns the version string; a wrong port fails cleanly.
- Two tables in one schema that both have an index called `idx_name`;
  `get_indexes` and `drop_index` target the right one.
- Two tables whose primary keys are both `<table>_pkey`; `get_columns` marks
  only each table's own key columns.
- A table without a primary key: `rowid` is hidden from `get_columns`, and
  update/delete by `rowid` work.
- A table with a `SERIAL` key: the inserted key survives a round trip through
  `insert_record` → `update_record` → `delete_record` with no precision loss.
- Composite foreign key.
- Enum column: read as label, write via cast.
- `JSONB`, `DECIMAL`, `BYTES`, `UUID`, `INET`, `INTERVAL`, `STRING[]` round
  trips.
- A view and a materialized view: listing, definition, refresh.
- A forced `40001`: two sessions update the same row in explicit
  transactions. An implicit-transaction statement retries and succeeds; a
  user transaction gets the error.
- `cancel` on `SELECT pg_sleep(30)` returns a cancelled error within a
  second.
- `EXPLAIN` and `EXPLAIN ANALYZE` on a join parse into a tree with
  `relation` and row counts.
- `ALTER PRIMARY KEY` on a populated table.

## CI matrix

`.github/workflows/ci.yml` runs the live tests against:

- `cockroachdb/cockroach:latest-v24.3`: the oldest supported LTS line
- `cockroachdb/cockroach:latest`: the newest release

When a line goes out of support, bump the first entry and the "Supported
versions" note in [README](./README.md). Behavior marked "verify on the CI
matrix" in these documents gets a live test before the code depends on it.

## Secure-mode tests (manual, before 1.0)

CI runs an insecure node. Before the first stable release, test by hand:

- A secure local cluster (`cockroach cert create-ca`, `create-node`,
  `create-client root`) with `verify-full` and client-certificate auth.
- `verify-ca` against a node certificate whose SAN does not include the host
  used to connect.
- A CockroachDB Cloud Basic cluster with `options=--cluster=...` in the URI
  and password auth.
- A connection through an SSH tunnel set up by the host.

Record the results in the release PR.

## Manual checks in Tabularis

`just dev-install`, then in the app:

1. CockroachDB appears in the database picker with its icon and color.
2. The connection form defaults to port `26257` and user `root`.
3. The schema tree shows databases, schemas, tables, views, materialized
   views; `system` and `crdb_internal` are hidden.
4. Editing a row in a table with a `SERIAL` key saves the right row.
5. Visual EXPLAIN shows the plan tree.

Take the registry screenshots during this pass and add them to `assets/` and
to `screenshots` in `.tabularium`.
