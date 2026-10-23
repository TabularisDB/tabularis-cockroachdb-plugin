# 01 · Connection

## Parameters from the host

Every RPC carries a `ConnectionParams` object in `params.params`. The fields
the driver uses:

| Field | Use |
| --- | --- |
| `host`, `port` | Any node, or a load balancer in front of the cluster. Default port `26257`. |
| `username`, `password` | Password auth. An empty password is valid for certificate auth and `--insecure` clusters. |
| `database` | Initial database. Default `defaultdb`. |
| `ssl_mode`, `ssl_ca`, `ssl_cert`, `ssl_key` | TLS, see below. |
| `connection_uri` | The raw URI when the user imported a connection string. Read it for parameters the decomposed fields cannot carry (`options`, `sslrootcert`, `application_name`). |
| `extra` | Plugin-owned fields and secrets, if a UI extension adds any (phase 7). |
| `ssh_*`, `k8s_*` | Tunnels are set up by the host; the driver receives the local endpoint in `host`/`port`. |

The scaffold's `ConnectionParams` in `src/models.rs` only reads `driver`,
`host`, `port`, `database`, `username`, `password` and `ssl_mode`. Extend it
with the TLS paths, `connection_uri` and `extra`.

## Connection strings

Accept three schemes and treat them the same way:

```text
postgresql://root@localhost:26257/defaultdb?sslmode=verify-full
postgres://root@localhost:26257/defaultdb
cockroachdb://root@localhost:26257/defaultdb?sslmode=disable
cockroach://root@localhost:26257/defaultdb
```

The manifest declares `cockroachdb` and `cockroach` in
`connection_uri_schemes`. `postgresql://` and `postgres://` belong to the
PostgreSQL driver in the host's scheme registry, so a pasted
`postgresql://` string only reaches this driver when the user has picked
CockroachDB first.

- The path is the database name. An empty path means `defaultdb`.
- A missing port means `26257`, not `5432`.
- Query parameters to honor:
  - `sslmode`: `disable`, `allow`, `prefer`, `require`, `verify-ca`, `verify-full`.
  - `sslrootcert`, `sslcert`, `sslkey`: file paths. They override the form's TLS paths.
  - `options`: passed verbatim as the startup `options` parameter. CockroachDB
    Cloud Basic/Serverless uses it for cluster routing:
    `options=--cluster%3D<routing-id>`. URL-decode it once.
  - `application_name`: overrides the `applicationName` setting.
  - `user`, `password`: fallbacks when the userinfo part is empty.
- Unknown parameters are ignored, not rejected.

Pure function, unit-tested: `parse_connection_uri(&str) -> Result<UriParts>`.

## TLS

| `ssl_mode` | Behavior |
| --- | --- |
| `disable` | Plain TCP. Only valid against `--insecure` clusters. |
| `prefer` (default) | Try TLS, fall back to plain TCP. Works for both secure and `--insecure` dev clusters. |
| `require` | TLS, no certificate verification. |
| `verify-ca` | Verify the chain against `ssl_ca` (or the platform store), but not the hostname. |
| `verify-full` | Verify chain and hostname. Required for CockroachDB Cloud. |

- `ssl_cert` + `ssl_key` enable client-certificate authentication, which
  CockroachDB uses for `root` on self-hosted secure clusters. Load PEM; accept
  PKCS#8 and PKCS#1 keys.
- With no `ssl_ca`, verify against the platform store
  (`rustls-platform-verifier`). CockroachDB Cloud certificates chain to a
  public CA.
- `verify-ca` needs a custom verifier that skips only the hostname check.
  This is easy to get wrong. Test it with a locally generated CA and a
  certificate whose SAN does not match.

## Pooling

- One pool per connection profile and database. The pool key includes `host`,
  `port`, `database`, `username`, every TLS field, `options`, and
  `application_name`. A key that leaves out TLS or routing fields hands one
  cluster's connections to another profile.
- Pool size comes from the `poolMaxSize` setting (default 10).
- Database switching opens a separate pool for the other database instead of
  running `SET database` on a pooled connection. `SET database = <db>` does
  work on CockroachDB, but a pooled connection with a changed database breaks
  the pool key.
- TCP keepalives on idle connections: idle 60 s, interval 10 s, 3 probes.
  Load balancers in front of a cluster (and CockroachDB Cloud) drop idle
  connections.
- Connect timeout: 10 s.

## Session bootstrap

Run once per new physical connection, in this order:

1. Startup parameters: `application_name` (setting `applicationName`, default
   `tabularis`), `client_encoding=UTF8`, and `options` when present.
2. `SELECT version()`. Parse the release (see below) and cache it on the pool.
3. `SELECT current_schema()`. If it returns NULL (the first `search_path`
   entry does not exist), fall back to `SELECT (current_schemas(false))[1]`,
   then to `public`.
4. If the host sent a schema for this call, `SET search_path TO <schema>`.

Use a different `application_name` suffix for the metadata pool and the
query pool (`tabularis` vs `tabularis metadata`). Sessions then show up
separately in `SHOW SESSIONS` and the DB Console.

`standard_conforming_strings` is always `on` in CockroachDB and cannot be
turned off. String escaping can rely on it.

## Version detection

`server_version` and `server_version_num` always report PostgreSQL `13.0.0`.
Never gate features on them.

```sql
SELECT version()
-- CockroachDB CCL v24.3.4 (x86_64-pc-linux-gnu, built 2025/01/15 ..., go1.22.8)
```

- Parse `v(\d+)\.(\d+)\.(\d+)` from the text into a `CrdbVersion { major,
  minor, patch }` with `Ord`. Keep the full string for `test_connection` and
  diagnostics.
- A `version()` without a `CockroachDB` prefix means the user pointed the
  driver at another server. `test_connection` must fail with a clear message
  that names the server it found and suggests the PostgreSQL driver.
- Cross-check, optional: `SELECT value FROM crdb_internal.node_build_info
  WHERE field = 'Version'`.

Features gated on the version, each checked through `CrdbVersion`:

| Feature | Minimum | Notes |
| --- | --- | --- |
| User-defined functions | v22.2 | `routines` capability |
| `aclexplode`, schema privileges | v22.2 | Before that, return no grants |
| Stored procedures (`CALL`) | v23.2 | |
| `READ COMMITTED` isolation | v23.2 | Also needs a cluster setting; see [05](./05-query-execution.md) |
| Triggers | v24.3 | `triggers` capability is `false` for now |
| `VECTOR` type | v24.2 | |
| Vector indexes | v25.2 | Verify on the CI matrix |

Pure function, unit-tested: `parse_crdb_version(&str) -> Option<CrdbVersion>`,
with fixtures from several releases and from a vanilla PostgreSQL `version()`.

## `test_connection` and `ping`

- `test_connection`: connect (outside the pool), run the bootstrap, and return
  `{ "success": true, "version": "<full version string>" }`. Return the
  server's error text otherwise, including the SQLSTATE.
- `ping`: `SELECT 1` on a pooled connection.

## Common errors

Map these to readable messages; keep the original text after them.

| Symptom | Likely cause |
| --- | --- |
| `node is running secure mode, SSL connection required` | `ssl_mode=disable` against a secure cluster |
| `tls: first record does not look like a TLS handshake` | TLS against an `--insecure` cluster |
| `codeParamsRoutingFailed` / `missing cluster identifier` | CockroachDB Cloud Basic without `options=--cluster=...` |
| `password authentication failed` with an empty password | Certificate auth expected; client cert missing |
| Connection refused on `5432` | Port left at the PostgreSQL default |
