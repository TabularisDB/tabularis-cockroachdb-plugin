# 05 · Query execution

## RPC surface

| Method | Purpose |
| --- | --- |
| `execute_query` | One statement, paginated result |
| `execute_query_batch` | A script: every statement on one connection, in order |
| `release_session` | The host closed an editor session; roll back and return its pinned connection |
| `cancel` | Notification: cancel the request with `params.id` (no response) |

Copy the session and cancellation design from the PostgreSQL plugin
(`session.rs`, `cancel.rs`), then apply the differences below.

## Transactions and isolation

- Default isolation is `SERIALIZABLE`. `READ COMMITTED` exists from v23.2,
  but only when the cluster setting
  `sql.txn.read_committed_isolation.enabled` is on. Never assume it.
- `BEGIN` / `COMMIT` / `ROLLBACK` from the user's script run as written.
- Writes issued by the driver itself (CRUD saves, multi-row edits) start with
  `BEGIN READ WRITE`, not `BEGIN`. A cluster or role can set
  `default_transaction_read_only = on`; a plain `BEGIN` then fails on the
  first write.
- **Check the session state before issuing transaction statements.** An
  extra `BEGIN` while the user has a transaction open, or a `COMMIT` /
  `ROLLBACK` the user did not ask for, aborts the user's whole transaction on
  CockroachDB. Use the connection's transaction status (`ReadyForQuery`:
  idle, in transaction, failed) before the driver adds any statement of its
  own.
- Report `in_transaction` to the host after each batch, from the same status.

## Retries (`SQLSTATE 40001`)

Under `SERIALIZABLE`, contention makes the server abort transactions with
`40001` (`restart transaction: TransactionRetryWithProtoRefreshError ...`).
CockroachDB retries some of these itself, but not after it has sent results
to the client. The driver has to handle the rest.

| Situation | What to do |
| --- | --- |
| Single statement, no open transaction (implicit transaction) | Retry up to `retryMaxAttempts` times, with exponential backoff and jitter (10 ms, 20 ms, 40 ms, ..., capped at 1 s). |
| Driver-owned write transaction (CRUD save) | Retry the whole transaction with the `SAVEPOINT cockroach_restart` protocol, below. |
| User's explicit transaction (`BEGIN` in the editor) | Do **not** retry. Return the error. The transaction is now aborted; only `ROLLBACK` works. |
| `40003` (`statement_completion_unknown`) | Never retry. The statement may or may not have committed; say so in the error. |

The retry protocol for driver-owned transactions:

```sql
BEGIN READ WRITE;
SAVEPOINT cockroach_restart;
-- statements
RELEASE SAVEPOINT cockroach_restart;
COMMIT;
-- on 40001 before COMMIT:
ROLLBACK TO SAVEPOINT cockroach_restart;
-- then run the statements again
```

Retries apply to statements whose effect does not depend on what the client
saw. Never retry a statement after part of its result has been streamed to
the host.

Pure function, unit-tested: `is_retryable(sqlstate, in_user_txn, streamed) ->
bool`.

## Statements that cannot run in a transaction

`execute_query_batch` runs statements in order on one connection. When the
host asks the driver to wrap a batch in a transaction, these statements run
outside the wrap, in autocommit:

- All schema changes (see [04](./04-ddl.md)).
- `SET CLUSTER SETTING ...`
- `BACKUP`, `RESTORE`, `IMPORT`, unless the statement says `DETACHED`
  (`WITH detached`), which makes it a job that can run in a transaction.
- `CREATE DATABASE`, `DROP DATABASE`.
- `CREATE INDEX CONCURRENTLY` (accepted, and online anyway).

Classify by the first keyword(s) after stripping comments and whitespace.
Unit-test the classifier with the forms above, upper and lower case, and with
leading comments.

## Session scope

- Schema: `SET search_path TO <schema>` when the host sends a schema.
- Timeout: `SET statement_timeout = '<ms>ms'` when the host sends one.
  `0` disables.
- Settings changed by the user in a session (`SET ...`) stay on the pinned
  connection. A connection returns to the pool only after `ROLLBACK` and
  `RESET ALL`. Otherwise the next borrower inherits the session's settings.
- Temporary tables need `experimental_enable_temp_tables = on` in the
  session. Do not enable it on the user's behalf; let the server error say so.

## Cancellation

- Handle the `cancel` notification: look up the running request by
  `params.id` and cancel it. Ignore unknown ids. Never write a response.
- Keep reading `stdin` while a request runs. A strictly sequential loop only
  sees the cancel after the long call has finished.
- Mechanism: `tokio-postgres`'s `CancelToken::cancel_query`, which sends a
  pgwire cancel request. CockroachDB supports it on the supported releases;
  verify behind a load balancer on the CI matrix, because the cancel request
  is a new TCP connection that may land on another node.
- Fallback when the pgwire cancel does not reach the right node:

  ```sql
  CANCEL QUERY (
    SELECT query_id FROM [SHOW CLUSTER STATEMENTS]
    WHERE application_name = $1 AND query = $2
  )
  ```

  Tag each query connection with a unique `application_name` suffix so the
  lookup cannot hit another client's query.

- A cancelled statement returns `57014`. Report "cancelled", not an error.

## Results and pagination

- `execute_query` with `page` / `page_size`: wrap a single `SELECT` as
  `SELECT * FROM (<query>) AS t LIMIT $1 OFFSET $2`, as the scaffold's
  `paginate` helper does. Leave non-`SELECT` statements alone.
- `total_count`: run `SELECT count(*) FROM (<query>) AS t` only when the host
  asks for it. On large tables a count scans the whole table; for table
  browsing, offer `estimated_row_count` from `SHOW TABLES` instead
  (see [02](./02-introspection.md)).
- Stream rows (`query_raw`) and stop at `page_size`. Do not buffer the whole
  result.
- Affected rows: report the command tag's count for `INSERT`, `UPDATE`,
  `DELETE`, `UPSERT`.
- `execution_time_ms`: client-measured wall time.

## Errors

Include the SQLSTATE, the server message, `DETAIL` and `HINT` in the
JSON-RPC error. CockroachDB's hints often include a link to the docs (for
example for retry errors and unsupported features); keep them.
