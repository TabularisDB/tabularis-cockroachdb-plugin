<div align="center">
  <img src="https://raw.githubusercontent.com/TabularisDB/tabularis/main/packages/web-ui/public/logo-sm.png" width="120" height="120" alt="Tabularis logo" />
  <img src="https://raw.githubusercontent.com/TabularisDB/tabularis-cockroachdb-plugin/main/assets/plus.svg" width="40" height="40" alt="plus" />
  <img src="https://raw.githubusercontent.com/TabularisDB/tabularis-cockroachdb-plugin/main/cockroachdb-icon.svg" width="120" height="120" alt="CockroachDB logo" />
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

> **Status: under development.** The repository was generated with
> `@tabularis/create-plugin` and the RPC handlers are still stubs. The
> `.tabularium` manifest already declares the capabilities of the finished
> driver. Implementation is planned in [`docs/architecture`](./docs/architecture/README.md).
>
> **Requires Tabularis v0.29.0 or later** (column comments and connection
> field overrides in the manifest).

## Table of Contents

- [Planned Features](#planned-features)
- [Connection Configuration](#connection-configuration)
- [Supported CockroachDB Data Types](#supported-cockroachdb-data-types)
- [Installation](#installation)
- [How It Works](#how-it-works)
- [Building from Source](#building-from-source)
- [Development](#development)
- [Architecture Documents](#architecture-documents)
- [Maintainers](#maintainers)
- [License](#license)

## Planned Features

- **CockroachDB detection and version gating**: reads `version()` /
  `crdb_internal.node_build_info` and enables features per server release.
- **Schema browsing**: databases, schemas, tables, views, materialized views,
  sequences, user-defined functions, indexes (including hash-sharded,
  partial, inverted, and vector indexes), foreign keys, and comments. Hidden
  columns such as `rowid` and `crdb_region` are excluded by default.
- **Query execution**: pooled connections, result pagination, query
  cancellation, and automatic retry of implicit transactions on serialization
  failures (`SQLSTATE 40001`).
- **EXPLAIN**: `EXPLAIN` and `EXPLAIN ANALYZE` rendered as visual plans,
  including distributed execution details.
- **Row editing**: type-aware binding for `STRING`, `INT8`, `DECIMAL`, `UUID`,
  `JSONB`, `BYTES`, arrays, `INET`, enums, and spatial types.
- **DDL generation**: CockroachDB-native `CREATE TABLE`, `ALTER PRIMARY KEY`,
  `table@index` index operations, `STORING`, hash-sharded and inverted
  indexes, and `unique_rowid()` / `gen_random_uuid()` keys.
- **Multi-region awareness**: table localities (`REGIONAL BY ROW`, `GLOBAL`),
  database regions, and survival goals in metadata.
- **TLS**: `disable`, `require`, `verify-ca`, and `verify-full` modes with a
  custom CA and client certificates, which CockroachDB uses for certificate
  authentication.

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
| `retryMaxAttempts` | `3` | Automatic retries of implicit transactions on `40001`. `0` disables retries. |
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
   [Releases](https://github.com/TabularisDB/tabularis-cockroachdb-plugin/releases).
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

Live-cluster tests go in `tests/live_db.rs`. CI runs them against the oldest
supported LTS line and the latest CockroachDB image once that file exists.

### Contributing: PR Titles & Versioning

PR titles must follow [Conventional Commits](https://www.conventionalcommits.org/)
(`type: subject`, `type(scope): subject`, or `type!: subject` for a breaking
change). CI enforces this on every PR. Add a `BREAKING CHANGE:` footer to the
PR description for breaking changes that don't fit in the title.

Every PR also needs exactly one `prerelease:alpha`, `prerelease:beta`,
`prerelease:rc`, or `prerelease:stable` label, so CI knows which release
channel to target when it suggests the next version. Until the driver is
feature-complete, use `prerelease:alpha`.

| PR title type | Version impact |
| --- | --- |
| `feat` | minor |
| `fix`, `refactor`, `perf` | patch |
| `docs`, `style`, `chore`, `test`, `ci`, `build` | none |
| any type with `!` or a `BREAKING CHANGE:` footer | major |

Releases are cut by pushing a `vX.Y.Z` tag. The tag must equal the
`.tabularium` version, and `CHANGELOG.md` must contain a `## [X.Y.Z]`
section, which becomes the release notes.

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
