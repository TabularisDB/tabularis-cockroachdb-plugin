//! Connection checks and query execution.
//!
//! Design notes (see `docs/architecture/05-query-execution.md`):
//! - A batch runs every statement on ONE connection, in order, so `BEGIN ...
//!   COMMIT` inside a script works. With a `session_id`, a run that leaves a
//!   transaction open keeps its connection pinned (`session.rs`).
//! - The transaction state after a run comes from the server
//!   (`SHOW TRANSACTION STATUS`), not from guessing at keywords. Keyword
//!   tracking only decides whether a statement may be retried.
//! - `40001` is retried only for a statement that runs alone outside any
//!   transaction (`retry.rs`).
//! - A connection goes back to the pool only outside a transaction, and after
//!   `RESET ALL` if the run changed session state or the search path.

use std::time::Instant;

use deadpool_postgres::Object;
use serde_json::{json, Value};

use crate::cancel;
use crate::client::{self, Connection, PoolKind};
use crate::error::{describe_pg_error, pg_sqlstate, PluginError};
use crate::extract;
use crate::models::{inner_params, ConnectionParams};
use crate::retry;
use crate::rpc::{error_response, not_implemented, ok_response};
use crate::session;
use crate::settings;
use crate::sqlkind::{self, TransactionEffect};
use crate::utils::identifiers::quote_identifier;
use crate::utils::pagination::paginate_query;

pub async fn test_connection(id: Value, params: &Value) -> Value {
    let conn = ConnectionParams::from_value(inner_params(params));
    let schema = params.get("schema").and_then(Value::as_str);
    match client::test_connection(&conn, schema).await {
        Ok(info) => ok_response(
            id,
            json!({ "success": true, "version": info.version_string }),
        ),
        Err(e) => error_response(id, e.code, &e.message),
    }
}

pub async fn ping(id: Value, params: &Value) -> Value {
    let conn = ConnectionParams::from_value(inner_params(params));
    match client::ping(&conn).await {
        Ok(()) => ok_response(id, Value::Null),
        Err(e) => error_response(id, e.code, &e.message),
    }
}

/// A statement failure: the text for the host, and the SQLSTATE (if the
/// server sent one) for the retry decision.
#[derive(Debug)]
pub struct QueryError {
    pub message: String,
    pub sqlstate: Option<String>,
}

impl From<tokio_postgres::Error> for QueryError {
    fn from(e: tokio_postgres::Error) -> Self {
        Self {
            message: describe_pg_error(&e),
            sqlstate: pg_sqlstate(&e),
        }
    }
}

/// Result shape the host reads as a `QueryResult`.
fn result_value(
    columns: Vec<String>,
    rows: Vec<Value>,
    affected: u64,
    truncated: bool,
    pagination: Value,
) -> Value {
    json!({
        "columns": columns,
        "rows": rows,
        "affected_rows": affected,
        "truncated": truncated,
        "pagination": pagination,
    })
}

/// Run one statement once. A statement that describes no columns is a
/// command (`affected_rows` from the command tag); otherwise its rows are
/// decoded. Pagination appends `LIMIT n+1 OFFSET m` to statements whose
/// grammar accepts it; the extra row tells the host there is another page.
/// Other statements are capped client-side by `limit`.
async fn exec_once(
    client: &Object,
    query: &str,
    limit: Option<u32>,
    page: u32,
) -> Result<Value, QueryError> {
    // Pagination rewrites the SQL, so it applies only to statements that read
    // and whose text can be rewritten safely (see `utils::pagination`). Anything
    // else is capped on this side instead.
    let page_sql = limit
        .filter(|_| sqlkind::supports_trailing_limit(query))
        .and_then(|l| paginate_query(query, l, page).map(|sql| (sql, l)));
    let paginated = page_sql.is_some();
    let (sql, page_size) = page_sql.unwrap_or_else(|| (query.to_string(), 0));
    let manual_limit = if paginated { None } else { limit };

    let stmt = client.prepare(&sql).await?;
    if stmt.columns().is_empty() {
        let affected = client.execute(&stmt, &[]).await?;
        return Ok(result_value(vec![], vec![], affected, false, Value::Null));
    }

    let columns: Vec<String> = stmt
        .columns()
        .iter()
        .map(|c| c.name().to_string())
        .collect();
    let rows = client.query(&stmt, &[]).await?;

    let cap = if paginated {
        Some(page_size as usize)
    } else {
        manual_limit.map(|l| l as usize)
    };
    let truncated = cap.is_some_and(|c| rows.len() > c);
    let kept = match cap {
        Some(c) if truncated => &rows[..c],
        _ => &rows[..],
    };
    let json_rows: Vec<Value> = kept
        .iter()
        .map(|r| Value::Array(extract::row_to_json(r)))
        .collect();
    let pagination = if paginated {
        json!({ "page": page, "page_size": page_size, "total_rows": null, "has_more": truncated })
    } else {
        Value::Null
    };
    Ok(result_value(columns, json_rows, 0, truncated, pagination))
}

/// [`exec_once`] with the `40001` retry policy. `in_txn_before` is whether the
/// batch had a transaction open when this statement started.
async fn exec_with_retry(
    client: &Object,
    query: &str,
    limit: Option<u32>,
    page: u32,
    in_txn_before: bool,
) -> Result<Value, QueryError> {
    let max_attempts = settings::get().retry_max_attempts;
    // `BEGIN`, `COMMIT` and friends are transaction control, never retried.
    let in_user_txn =
        in_txn_before || sqlkind::transaction_effect(query) != TransactionEffect::None;
    let mut attempt = 0;
    loop {
        match exec_once(client, query, limit, page).await {
            Err(e)
                if attempt < max_attempts
                    && retry::is_retryable(e.sqlstate.as_deref(), in_user_txn, false) =>
            {
                tokio::time::sleep(retry::backoff_delay(attempt, retry::jitter_from_clock())).await;
                attempt += 1;
            }
            other => return other,
        }
    }
}

/// Whether the connection still has a transaction open, from the server. A
/// failed statement leaves it `Aborted`, which still has to be rolled back.
/// Falls back to `guess` if the server cannot answer.
async fn transaction_open(client: &Object, guess: bool) -> bool {
    match client.simple_query("SHOW TRANSACTION STATUS").await {
        Ok(messages) => messages
            .iter()
            .find_map(|m| match m {
                tokio_postgres::SimpleQueryMessage::Row(r) => r.get(0).map(|s| s != "NoTxn"),
                _ => None,
            })
            .unwrap_or(guess),
        Err(_) => guess,
    }
}

/// Run `queries` on one connection, reusing the session's pinned connection
/// when it left a transaction open, and re-pinning it if one is still open
/// after. Returns one result object per statement plus whether a transaction
/// is still open.
///
/// A run that leaves a transaction open with no session to pin it to is
/// rolled back: the pool recycles with `RecyclingMethod::Fast`, so the next
/// borrower would otherwise inherit the transaction and its locks.
async fn run_batch(
    conn_params: &ConnectionParams,
    queries: &[String],
    limit: Option<u32>,
    page: u32,
    schema: Option<&str>,
    session_id: Option<&str>,
    request_id: &Value,
) -> Result<(Vec<Value>, bool), PluginError> {
    // Everything that can fail before a connection is in hand happens first, so
    // an error can never drop a pinned connection (and its open transaction)
    // into the pool.
    let entry = client::get_pool(conn_params, PoolKind::Query)?;
    let connection = Connection::resolve(conn_params, &settings::get())?;

    // Held for the whole run, so an overlapping call for the same session waits.
    let mut slot = match session_id {
        Some(sid) => Some(session::lock(sid).await),
        None => None,
    };
    let pinned = slot.as_mut().and_then(|s| s.take());
    let reused = pinned.is_some();
    // A pinned connection exists only because its transaction is still open.
    let mut in_transaction = reused;
    // Session state set by this or any earlier run that has not been reset
    // yet. It travels with a pinned connection: a `COMMIT` run applies no
    // schema and runs no `SET`, but it is the run that frees the connection.
    let mut dirty = pinned.as_ref().is_some_and(|(_, dirty)| *dirty);

    let client = match pinned {
        Some((c, _)) => c,
        None => entry.pool.get().await.map_err(client::pool_error)?,
    };
    let guard = cancel::CancelGuard::register(request_id, &client, connection);

    // On a reused connection the search path would change inside the open
    // transaction, and what the rest of it sees.
    if let Some(s) = schema.filter(|s| !s.is_empty() && !reused) {
        let sql = format!("SET search_path TO {}", quote_identifier(s, '"'));
        client.batch_execute(&sql).await?;
        dirty = true;
    }

    let mut results = Vec::with_capacity(queries.len());
    for query in queries {
        let start = Instant::now();
        let outcome = exec_with_retry(&client, query, limit, page, in_transaction).await;
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

        in_transaction = sqlkind::transaction_effect(query)
            .in_transaction_after(outcome.is_ok(), in_transaction);
        if outcome.is_ok() && sqlkind::sets_session_state(query) {
            dirty = true;
        }
        results.push(match outcome {
            Ok(result) => {
                json!({ "result": result, "error": null, "execution_time_ms": elapsed_ms })
            }
            Err(e) => {
                json!({ "result": null, "error": e.message, "execution_time_ms": elapsed_ms })
            }
        });
    }
    // A cancel racing the end of the run must not target a finished statement.
    drop(guard);

    in_transaction = transaction_open(&client, in_transaction).await;
    if in_transaction {
        match slot.as_mut() {
            // Keep the flag with the connection: the run that ends the
            // transaction may be a bare `COMMIT` that knows nothing about it.
            Some(slot) => slot.pin(client, dirty),
            None => session::rollback_and_release(client).await,
        }
    } else if dirty {
        // The pool does not reset connections: undo `SET`s and the search path
        // before the next borrower gets this one. If that fails the connection
        // is not trustworthy, so close it rather than return it.
        if client.batch_execute("RESET ALL").await.is_err() {
            drop(Object::take(client));
        }
    }
    Ok((results, in_transaction))
}

fn limit_param(params: &Value) -> Option<u32> {
    params
        .get("limit")
        .and_then(Value::as_u64)
        .map(|v| v.min(u64::from(u32::MAX)) as u32)
}

fn page_param(params: &Value) -> u32 {
    params
        .get("page")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .clamp(1, u64::from(u32::MAX)) as u32
}

pub async fn execute_query(id: Value, params: &Value) -> Value {
    let conn = ConnectionParams::from_value(inner_params(params));
    let query = params.get("query").and_then(Value::as_str).unwrap_or("");
    let schema = params.get("schema").and_then(Value::as_str);
    let session_id = params.get("session_id").and_then(Value::as_str);
    let queries = [query.to_string()];

    match run_batch(
        &conn,
        &queries,
        limit_param(params),
        page_param(params),
        schema,
        session_id,
        &id,
    )
    .await
    {
        Ok((mut results, in_transaction)) => {
            let statement = results.pop().unwrap_or(Value::Null);
            let error = statement
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string);
            match (session_id, error) {
                // A session-aware reply still carries the state a failed COMMIT left.
                (Some(_), Some(e)) => {
                    ok_response(id, json!({ "error": e, "in_transaction": in_transaction }))
                }
                (Some(_), None) => ok_response(
                    id,
                    json!({ "result": statement.get("result").cloned().unwrap_or(Value::Null), "in_transaction": in_transaction }),
                ),
                (None, Some(e)) => error_response(id, -32603, &e),
                (None, None) => {
                    ok_response(id, statement.get("result").cloned().unwrap_or(Value::Null))
                }
            }
        }
        Err(e) => error_response(id, e.code, &e.message),
    }
}

pub async fn execute_query_batch(id: Value, params: &Value) -> Value {
    let conn = ConnectionParams::from_value(inner_params(params));
    let queries: Vec<String> = params
        .get("queries")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let schema = params.get("schema").and_then(Value::as_str);
    let session_id = params.get("session_id").and_then(Value::as_str);

    match run_batch(
        &conn,
        &queries,
        limit_param(params),
        page_param(params),
        schema,
        session_id,
        &id,
    )
    .await
    {
        // Only a session-aware call gets the richer shape; a host that sent no
        // session_id still receives the bare array it expects.
        Ok((results, in_transaction)) => match session_id {
            Some(_) => ok_response(
                id,
                json!({ "results": results, "in_transaction": in_transaction }),
            ),
            None => ok_response(id, json!(results)),
        },
        Err(e) => error_response(id, e.code, &e.message),
    }
}

/// The host closed an editor session: roll back and return its connection.
pub async fn release_session(id: Value, params: &Value) -> Value {
    match params.get("session_id").and_then(Value::as_str) {
        Some(sid) => {
            session::release(sid).await;
            ok_response(id, json!({ "released": true }))
        }
        None => error_response(id, -32602, "session_id is required"),
    }
}

/// `cancel`: stop the in-flight statement of the request named by `params.id`.
/// Unknown ids are ignored. As a notification it gets no reply (see `rpc.rs`).
pub async fn cancel(id: Value, params: &Value) -> Value {
    match params.get("id") {
        Some(target) if !target.is_null() => {
            cancel::cancel(target).await;
            ok_response(id, Value::Null)
        }
        _ => error_response(id, -32602, "id is required"),
    }
}

/// Phase 6.
pub async fn explain_query(id: Value, _params: &Value) -> Value {
    not_implemented(id, "explain_query")
}
