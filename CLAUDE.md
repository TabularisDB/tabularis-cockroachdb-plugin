# CLAUDE.md

## Status

Alpha (`0.1.0-alpha.1`). Connection and TLS, schema browsing, result decoding,
query execution (batches, sessions, cancellation, retries) and row editing
with BLOBs are implemented and tested. Still stubs that return `-32601`: the
DDL helpers and view mutations, `explain_query`, and the phase 7 metadata
(materialized views, sequences, routines, schema snapshot). The `.tabularium`
manifest declares the finished driver's capabilities, so those features show
errors in the UI until they land. Tracked in issue #16. What to build, and in
which order, is specified in `docs/architecture/` (start at
`docs/architecture/README.md`).

## Build & Test

```bash
cargo build              # Debug build
cargo build --release    # Release build
cargo test               # Unit tests
cargo clippy --all-targets -- -D warnings  # Lint
cargo fmt --all          # Format
just dev-install         # Build and install into the local Tabularis plugins dir
```

Tests that need a server skip without `COCKROACH_HOST` (Rust reports a skip as
a pass). CI sets `REQUIRE_LIVE_DB=1` to make a missing host a failure; run the
same way locally with
`COCKROACH_HOST=127.0.0.1 REQUIRE_LIVE_DB=1 cargo test`.

Local cluster for manual and live tests:

```bash
docker run -d --name crdb -p 26257:26257 -p 8080:8080 \
  cockroachdb/cockroach:latest start-single-node --insecure
```

## Architecture Rules

- All RPC handlers return `serde_json::Value`; dispatch lives in `src/rpc.rs`,
  handlers in `src/handlers/` by domain (metadata, query, crud, ddl).
- CockroachDB access goes through `src/client.rs`. The pool cache key must
  include every TLS parameter and the `options` / cluster routing value, not
  just `host:port:database:user`.
- Never assume PostgreSQL catalog behavior. CockroachDB's `pg_catalog` is an
  emulation: several columns are always empty or zero. Each catalog query in
  `docs/architecture/02-introspection.md` notes the source that is reliable.
- Treat `SQLSTATE 40001` as retryable only for implicit (single-statement)
  transactions. Inside a user `BEGIN`, surface the error.
- Never write to `stdout` outside the JSON-RPC response loop in `main.rs`.
  Any stray write corrupts the protocol stream. Log to stderr.

## Key Patterns

- **Adding an RPC method**: add it to `rpc.rs`'s dispatch table, implement it
  in the matching `handlers/` module, and check whether a capability flag in
  `.tabularium` depends on it (`docs/architecture/08-capabilities.md`).
- **Version gates**: features that depend on the server release (vector
  indexes, triggers, stored procedures, `ALTER PRIMARY KEY` details) check the
  parsed server version, never a feature probe that runs DDL.
- **Testing**: pure logic (SQL builders, value binding, version parsing, URL
  parsing, EXPLAIN parsing) gets unit tests in a sibling `_tests.rs` file.
  Behavior that depends on the server goes in `tests/live_db.rs`, which CI
  runs against a real CockroachDB node.
- **PR titles and versioning**: Conventional Commits titles plus one
  `prerelease:alpha|beta|rc|stable` label, both enforced by CI. See the
  README's "Contributing: PR Titles & Versioning".
