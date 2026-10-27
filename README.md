<div align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/TabularisDB/tabularis/main/.github/assets/banner-dark.png" />
    <img src="https://raw.githubusercontent.com/TabularisDB/tabularis/main/.github/assets/banner-light.png" alt="tabularis — the database client your AI agent can actually use" width="600" />
  </picture>

  <br/>

  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/TabularisDB/tabularis-cockroachdb-plugin/main/assets/cockroachdb-banner-dark.png" />
    <img src="https://raw.githubusercontent.com/TabularisDB/tabularis-cockroachdb-plugin/main/assets/cockroachdb-banner-light.png" alt="Native CockroachDB support" width="600" />
  </picture>
</div>

# tabularis-cockroachdb-plugin

<p align="center">

![Release](https://img.shields.io/github/v/release/TabularisDB/tabularis-cockroachdb-plugin?include_prereleases&style=flat)
![Downloads](https://img.shields.io/github/downloads/TabularisDB/tabularis-cockroachdb-plugin/total.svg?style=flat)
![CI](https://github.com/TabularisDB/tabularis-cockroachdb-plugin/workflows/CI/badge.svg)

</p>

A [CockroachDB](https://www.cockroachlabs.com/) driver plugin for
[Tabularis](https://github.com/TabularisDB/tabularis), the open-source database
client.

This plugin connects Tabularis to CockroachDB through a standalone Rust
executable that speaks JSON-RPC 2.0 over stdio. CockroachDB uses the PostgreSQL
wire protocol, but its catalog, type system, DDL, transaction model, and
`EXPLAIN` output differ from PostgreSQL in ways that a plain PostgreSQL driver
gets wrong. This plugin handles those differences natively.

> **Status: alpha (`0.1.0-alpha.1`).** Connecting, browsing the schema, running
> queries and editing rows work. DDL generation, `EXPLAIN` and a few metadata
> views are not implemented yet; see [Known limitations](#known-limitations).
> The `.tabularium` manifest declares the capabilities of the finished driver,
> so a few features it advertises still return an error until they land.
> Implementation follows [`docs/architecture`](./docs/architecture/README.md).
>
> **Requires Tabularis v0.29.0 or later** (column comments and connection
> field overrides in the manifest). Tested against CockroachDB v24.3 (the
> oldest supported LTS line) and the latest release.

## Table of Contents

- [Features](#features)
- [Known limitations](#known-limitations)
- [Connection Configuration](#connection-configuration)
- [Supported CockroachDB Data Types](#supported-cockroachdb-data-types)
- [Installation](#installation)
- [How It Works](#how-it-works)
- [Building from Source](#building-from-source)
- [Development](#development)
- [Architecture Documents](#architecture-documents)
- [Maintainers](#maintainers)
- [License](#license)

## Features

Available in this release:

- **Connection and TLS**: pooled connections, `disable`, `prefer`, `require`,
  `verify-ca` and `verify-full` modes with a custom CA and client certificates
  (PKCS#8, PKCS#1 and SEC1 keys, as `cockroach cert create-client` writes),
  `cockroachdb://`, `cockroach://`, `postgresql://` and `postgres://` connection
  strings, and `options=--cluster=...` passed through for CockroachDB Cloud.
  The plugin reads the real server version from `version()` and refuses to
  connect to a server that is not CockroachDB.
- **Schema browsing**: databases, schemas, tables (with comments and row
  estimates), views, columns (canonical types, comments, primary keys), indexes
  (key, `STORING`, expression and hash-sharded columns), foreign keys including
  composite ones, and the `SHOW CREATE TABLE` DDL. Hidden columns such as
  `rowid` and `crdb_region` are excluded by default.
- **Query execution**: pagination that understands comments, strings and
  `LIMIT`/`OFFSET`, script batches, transactions that span several runs of an
  editor tab, query cancellation, and automatic retry of implicit
  transactions on serialization failures (`SQLSTATE 40001`). Errors carry the
  SQLSTATE, `DETAIL` and `HINT`. `INT8` values beyond JavaScript's safe range
  arrive as strings, so `unique_rowid()` keys are never rounded.
- **Row editing**: insert, update and delete from the data grid with
  type-aware binding for `STRING`, `INT8`, `DECIMAL`, `UUID`, `JSONB`,
  `BYTES`, arrays, `INET`, enums and more. Composite keys, `NULL` keys, tables
  without a primary key (through the hidden `rowid`) and connections that
  default to read-only all work. An edit whose key would change more than one
  row is refused, and editing a row that no longer exists is reported as a
  conflict, not as a success.
- **BLOBs**: preview a `BYTES` cell, save it to a file, and upload a file into
  it (up to the host's `maxBlobSize`).

Planned (see [Known limitations](#known-limitations)):

- **EXPLAIN**: `EXPLAIN` and `EXPLAIN ANALYZE` rendered as visual plans,
  including distributed execution details.
- **DDL generation**: CockroachDB-native `CREATE TABLE`, `ALTER PRIMARY KEY`,
  `table@index` index operations, `STORING`, hash-sharded and inverted
  indexes, and `unique_rowid()` / `gen_random_uuid()` keys, plus view
  creation, alteration and removal.
- **More metadata**: materialized views, sequences, user-defined functions,
  multi-region awareness (table localities, database regions, survival goals)
  and the schema snapshot behind the ER diagram.

## Known limitations

This is an alpha. These gaps are known and tracked in
[#16](https://github.com/TabularisDB/tabularis-cockroachdb-plugin/issues/16):

- **`EXPLAIN` is not implemented.** The explain button returns a
  "Method not found" error.
- **DDL helpers are not implemented**: `get_create_table_sql`,
  `get_add_column_sql`, `get_alter_column_sql`, `get_create_index_sql`,
  `get_create_foreign_key_sql`, `drop_index`, `drop_foreign_key` and the view
  mutations (`create_view`, `alter_view`, `drop_view`). The manifest already
  advertises `manage_tables`, `alter_column` and `create_foreign_keys`, so the
  UI offers them and they fail with "Method not found". Running DDL as SQL in
  the query editor works, and `SHOW CREATE TABLE` browsing works.
- **Not yet listed in the sidebar or the ER diagram**: materialized views,
  sequences, routines, triggers, multi-region metadata and the schema
  snapshot.
- **Untested here**: `options=--cluster=...` against a real CockroachDB Cloud
  cluster, query cancellation behind a load balancer (the pgwire cancel
  request may reach another node), and a real `40001` retry of an implicit
  statement, which could not be provoked on the tested releases. The retry
  rules themselves have unit tests.
- **Pagination of an unusual `LIMIT` form** (`LIMIT ALL`, `FETCH FIRST`) wraps
  the statement in a subquery. CockroachDB orders such a subquery in practice,
  but SQL does not guarantee it.

## Connection Configuration

| Field | Default | Notes |
| --- | --- | --- |
| Host | `localhost` | Any node or a load balancer in front of the cluster. |
| Port | `26257` | CockroachDB's SQL port, not PostgreSQL's `5432`. |
| Username | `root` | |
| Password | | Leave empty for certificate authentication or `--insecure` dev clusters. |
| Database | `defaultdb` | |
| SSL mode | `prefer` | Use `verify-full` for CockroachDB Cloud and `disable` for `--insecure` dev clusters. |

Connection strings are accepted with the `cockroachdb://`, `cockroach://`,
`postgresql://` and `postgres://` schemes:

```text
cockroachdb://root@localhost:26257/defaultdb?sslmode=disable
postgresql://root@localhost:26257/defaultdb?sslmode=verify-full
postgresql://user:pass@free-tier.gcp-us-central1.cockroachlabs.cloud:26257/defaultdb?sslmode=verify-full&options=--cluster%3Dmy-cluster-123
```

The `options=--cluster=<routing-id>` parameter is passed through unchanged for
CockroachDB Cloud (serverless/Basic) clusters.

`cockroachdb://` and `cockroach://` select this driver automatically. A
`postgresql://` string is handled by this driver once CockroachDB is picked as
the connection type.

### Plugin settings

| Key | Default | Purpose |
| --- | --- | --- |
| `poolMaxSize` | `10` | Maximum pooled connections per connection profile. |
| `retryMaxAttempts` | `3` | Automatic retries, on `40001`, of statements run outside a transaction and of row edits. `0` disables retries. |
| `applicationName` | `tabularis` | `application_name` reported to the cluster. |

## Supported CockroachDB Data Types

The column editor offers the types CockroachDB actually stores, using their
canonical names: `INT2`, `INT4`, `INT8`, `FLOAT4`, `FLOAT8`, `DECIMAL`,
`STRING`, `VARCHAR`, `CHAR`, `BOOL`, `DATE`, `TIME`, `TIMETZ`, `TIMESTAMP`,
`TIMESTAMPTZ`, `INTERVAL`, `UUID`, `JSONB`, `BYTES`, `BIT`, `VARBIT`, `INET`,
`GEOMETRY`, `GEOGRAPHY`, `TSVECTOR`, `TSQUERY`, `VECTOR`, and `OID`. Arrays of
any non-array type and user-defined `ENUM` types are read and written as well.

PostgreSQL aliases are accepted on input and normalized: `INTEGER` and `INT`
become `INT8`, `TEXT` becomes `STRING`, `JSON` becomes `JSONB`, `BYTEA` becomes
`BYTES`, and `SERIAL` becomes an `INT8` column with a `unique_rowid()` default.
See [`docs/architecture/03-types.md`](./docs/architecture/03-types.md).

## Installation

### From the Tabularium registry

Once a release is published, open **Settings → Plugins** in Tabularis, search
for **CockroachDB**, and click **Install**.

### Manual installation

1. Download the archive for your platform from
   [Releases](https://github.com/TabularisDB/tabularis-cockroachdb-plugin/releases):
   `cockroachdb-plugin-linux-x64.zip`, `-linux-arm64.zip`, `-darwin-x64.zip`,
   `-darwin-arm64.zip` or `-win-x64.zip`. Pre-releases such as `0.1.0-alpha.1`
   are marked as such on that page.
2. Extract it into the Tabularis plugins folder:

| OS | Path |
| --- | --- |
| Linux | `~/.local/share/tabularis/plugins/drivers/cockroachdb/` |
| macOS | `~/Library/Application Support/tabularis/plugins/drivers/cockroachdb/` |
| Windows | `%APPDATA%\tabularis\plugins\drivers\cockroachdb\` |

1. Restart Tabularis, or toggle the plugin in **Settings → Plugins**.

## How It Works

```text
Tabularis (host) ──stdin──▶ cockroachdb-plugin ──pgwire/TLS──▶ CockroachDB cluster
                 ◀─stdout──  (JSON-RPC 2.0)
```

Tabularis starts the executable and exchanges one JSON-RPC message per line.
The plugin keeps a connection pool per profile and translates each RPC
(`get_tables`, `execute_query`, `explain_query`, ...) into CockroachDB SQL.

## Building from Source

### Prerequisites

- Rust stable (pinned in `rust-toolchain.toml`)
- [`just`](https://github.com/casey/just) 1.44 or newer (optional)
- Docker, for a local CockroachDB node

### Build and install locally

```bash
just build          # cargo build
just release        # cargo build --release
just dev-install    # build and copy into the Tabularis plugins folder
```

### Local CockroachDB node

```bash
docker run -d --name crdb -p 26257:26257 -p 8080:8080 \
  cockroachdb/cockroach:latest start-single-node --insecure
```

Connect with host `localhost`, port `26257`, user `root`, database
`defaultdb`, and SSL mode `disable`. The DB Console is at
<http://localhost:8080>.

## Development

```bash
just test           # unit tests
just lint           # clippy -D warnings
just fmt            # rustfmt
just repl           # simulate Tabularis JSON-RPC calls over stdio
```

Manual JSON-RPC call:

```bash
echo '{"jsonrpc":"2.0","method":"initialize","id":1}' | ./target/debug/cockroachdb-plugin
```

Tests that need a server run against a real CockroachDB node: the integration
tests in `tests/live_db.rs`, and the decoding and binding tests inside the
crate. Point them at the local node from above:

```bash
COCKROACH_HOST=127.0.0.1 cargo test
```

Without `COCKROACH_HOST` those tests skip, and Rust reports a skip as a pass.
CI therefore also sets `REQUIRE_LIVE_DB=1`, which turns a missing host into a
failure, and runs everything against the oldest supported LTS line and the
latest CockroachDB image. See
[`docs/architecture/09-testing.md`](./docs/architecture/09-testing.md).

### Contributing: PR Titles & Versioning

PR titles must follow [Conventional Commits](https://www.conventionalcommits.org/)
(`type: subject`, `type(scope): subject`, or `type!: subject` for a breaking
change). CI enforces this on every PR. Add a `BREAKING CHANGE:` footer to the
PR description for breaking changes that don't fit in the title.

Every PR also needs exactly one `prerelease:alpha`, `prerelease:beta`,
`prerelease:rc`, or `prerelease:stable` label, so CI knows which release
channel to target when it suggests the next version. Until the driver is
feature-complete (DDL generation and `EXPLAIN` are still open), use
`prerelease:alpha`.

| PR title type | Version impact |
| --- | --- |
| `feat` | minor |
| `fix`, `refactor`, `perf` | patch |
| `docs`, `style`, `chore`, `test`, `ci`, `build` | none |
| any type with `!` or a `BREAKING CHANGE:` footer | major |

Releases are cut by pushing a `vX.Y.Z` tag (a pre-release such as
`v0.1.0-alpha.1` is flagged as one automatically). The tag must equal the
`.tabularium` version, and `CHANGELOG.md` must contain a `## [X.Y.Z]`
section, which becomes the release notes. To cut a release:

1. In one PR, set the version in `.tabularium` and `Cargo.toml` (then run
   `cargo build` so `Cargo.lock` follows) and move the `[Unreleased]` notes
   into a new `## [X.Y.Z]` section of `CHANGELOG.md`.
2. After it merges, tag that commit and push the tag:
   `git tag vX.Y.Z && git push origin vX.Y.Z`.

## Architecture Documents

[`docs/architecture`](./docs/architecture/README.md) specifies what to build,
area by area: connection and detection, catalog introspection, types, DDL,
query execution and transactions, EXPLAIN, CRUD and export, capabilities, and
the test plan.

## [Changelog](./CHANGELOG.md)

## Maintainers

- [@1-DARK](https://github.com/1-DARK)

## License

Apache-2.0. CockroachDB is a trademark of Cockroach Labs, Inc. This project is
not affiliated with or endorsed by Cockroach Labs.
