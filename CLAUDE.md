# CLAUDE.md

## Status

Scaffolded with `@tabularis/create-plugin`; handlers are stubs. The
`.tabularium` manifest already declares the target capabilities, so features
whose handlers still return `-32601` show errors in the UI until they are
implemented. What to build, and in which order, is specified in
`docs/architecture/` (start at `docs/architecture/README.md`).

## Build & Test

```bash
cargo build              # Debug build
cargo build --release    # Release build
cargo test               # Unit tests
cargo clippy --all-targets -- -D warnings  # Lint
cargo fmt --all          # Format
just dev-install         # Build and install into the local Tabularis plugins dir
```

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
