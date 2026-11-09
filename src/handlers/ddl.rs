//! Schema management.
//!
//! Two kinds of method, because the host uses them differently (read from the
//! host's code, which differs from the plugin guide in places):
//!
//! - **Generators** (`get_create_table_sql`, `get_add_column_sql`,
//!   `get_alter_column_sql`, `get_create_index_sql`,
//!   `get_create_foreign_key_sql`) return the SQL as an array of statements. The
//!   host previews them and then runs each one as its own `execute_query` call,
//!   on whatever connection is free and without a session. That is why a
//!   `SET` cannot be returned as a statement of its own: it would not reach the
//!   next one.
//! - **Executors** (`drop_index`, `drop_foreign_key`, `create_view`,
//!   `alter_view`, `drop_view`) run the change themselves and reply `null`.
//!
//! Every statement runs on its own, never inside a wrapping `BEGIN`: schema
//! changes are asynchronous jobs in CockroachDB and mixing them into a
//! transaction has restrictions. The one exception is replacing a view whose
//! columns change, which must drop and create it atomically.

use deadpool_postgres::Object;
use serde_json::{json, Value};

use crate::client::{self, PoolKind};
use crate::ddl::{self, ColumnDef};
use crate::error::PluginError;
use crate::handlers::query::QueryError;
use crate::models::{inner_params, ConnectionParams};
use crate::retry;
use crate::rpc::result_response;
use crate::settings;

// ---- request fields ---------------------------------------------------------------------

fn text<'a>(params: &'a Value, keys: &[&str]) -> Result<&'a str, PluginError> {
    keys.iter()
        .find_map(|k| params.get(*k).and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| PluginError::invalid_params(format!("missing parameter '{}'", keys[0])))
}

fn optional<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

fn column(params: &Value, key: &str) -> Result<ColumnDef, PluginError> {
    let v = params
        .get(key)
        .ok_or_else(|| PluginError::invalid_params(format!("missing parameter '{key}'")))?;
    ColumnDef::from_value(v)
}

fn connection(params: &Value) -> ConnectionParams {
    ConnectionParams::from_value(inner_params(params))
}

/// The schema named in the request, or the session's current schema. A
/// generator should work even when the server cannot be reached (a missing
/// keychain secret, say), so it falls back to `public`.
async fn schema_or_default(params: &Value, conn: &ConnectionParams) -> String {
    if let Some(s) = optional(params, "schema") {
        return s.to_string();
    }
    if let Ok(entry) = client::get_pool(conn, PoolKind::Metadata) {
        if entry.pool.get().await.is_ok() {
            if let Some(info) = entry.server_info() {
                return info.current_schema;
            }
        }
    }
    "public".to_string()
}

fn statements(list: Vec<String>) -> Value {
    json!(list)
}

// ---- generators ---------------------------------------------------------------------------

pub async fn get_create_table_sql(id: Value, params: &Value) -> Value {
    result_response(id, create_table(params).await)
}

async fn create_table(params: &Value) -> Result<Value, PluginError> {
    let table = text(params, &["table_name", "table"])?;
    let columns = params
        .get("columns")
        .and_then(Value::as_array)
        .ok_or_else(|| PluginError::invalid_params("missing parameter 'columns'"))?
        .iter()
        .map(ColumnDef::from_value)
        .collect::<Result<Vec<_>, _>>()?;
    let schema = schema_or_default(params, &connection(params)).await;
    ddl::build_create_table(&schema, table, &columns).map(statements)
}

pub async fn get_add_column_sql(id: Value, params: &Value) -> Value {
    result_response(id, add_column(params).await)
}

async fn add_column(params: &Value) -> Result<Value, PluginError> {
    let table = text(params, &["table"])?;
    let col = column(params, "column")?;
    let conn = connection(params);
    let schema = schema_or_default(params, &conn).await;
    // Making the new column part of the key extends the table's current key, so
    // that key has to be read. Nothing else here needs the server.
    let existing_pk = if col.is_pk {
        primary_key_columns(&conn, &schema, table).await?
    } else {
        Vec::new()
    };
    ddl::build_add_column(&schema, table, &col, &existing_pk).map(statements)
}

pub async fn get_alter_column_sql(id: Value, params: &Value) -> Value {
    result_response(id, alter_column(params).await)
}

async fn alter_column(params: &Value) -> Result<Value, PluginError> {
    let table = text(params, &["table"])?;
    let old = column(params, "old_column")?;
    let new = column(params, "new_column")?;
    let schema = schema_or_default(params, &connection(params)).await;
    ddl::build_alter_column(&schema, table, &old, &new).map(statements)
}

pub async fn get_create_index_sql(id: Value, params: &Value) -> Value {
    result_response(id, create_index(params).await)
}

async fn create_index(params: &Value) -> Result<Value, PluginError> {
    let table = text(params, &["table"])?;
    let index_name = params
        .get("index_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let columns: Vec<String> = params
        .get("columns")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let unique = params
        .get("is_unique")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let schema = schema_or_default(params, &connection(params)).await;
    ddl::build_create_index(&schema, table, index_name, &columns, unique)
        .map(|s| statements(vec![s]))
}

pub async fn get_create_foreign_key_sql(id: Value, params: &Value) -> Value {
    result_response(id, create_foreign_key(params).await)
}

async fn create_foreign_key(params: &Value) -> Result<Value, PluginError> {
    let schema = schema_or_default(params, &connection(params)).await;
    ddl::build_create_foreign_key(
        &schema,
        text(params, &["table"])?,
        text(params, &["fk_name", "constraint_name"])?,
        text(params, &["column"])?,
        text(params, &["ref_table"])?,
        text(params, &["ref_column"])?,
        optional(params, "on_delete"),
        optional(params, "on_update"),
    )
    .map(|s| statements(vec![s]))
}

/// Columns of the table's declared primary key, in key order. A table keyed by
/// the hidden `rowid` has none.
async fn primary_key_columns(
    conn: &ConnectionParams,
    schema: &str,
    table: &str,
) -> Result<Vec<String>, PluginError> {
    let entry = client::get_pool(conn, PoolKind::Metadata)?;
    let client = entry.pool.get().await.map_err(client::pool_error)?;
    let rows = client
        .query(
            "SELECT kcu.column_name::STRING \
             FROM information_schema.table_constraints tc \
             JOIN information_schema.key_column_usage kcu \
               ON tc.constraint_name = kcu.constraint_name \
              AND tc.table_schema = kcu.table_schema AND tc.table_name = kcu.table_name \
             WHERE tc.constraint_type = 'PRIMARY KEY' AND tc.table_schema = $1 AND tc.table_name = $2 \
               AND NOT EXISTS (SELECT 1 FROM information_schema.columns c \
                               WHERE c.table_schema = kcu.table_schema AND c.table_name = kcu.table_name \
                                 AND c.column_name = kcu.column_name AND c.is_hidden = 'YES') \
             ORDER BY kcu.ordinal_position",
            &[&schema, &table],
        )
        .await
        .map_err(|e| PluginError::internal(format!("cannot read the primary key of \"{schema}\".\"{table}\": {e}")))?;
    Ok(rows.iter().map(|r| r.get(0)).collect())
}

// ---- executors ------------------------------------------------------------------------------

fn query_error(e: QueryError) -> PluginError {
    PluginError::internal(e.message)
}

/// Run one statement on its own (autocommit), retrying `40001`.
async fn run_statement(client: &Object, sql: &str) -> Result<(), QueryError> {
    let max = settings::get().retry_max_attempts;
    let mut tries = 0;
    loop {
        match client.batch_execute(sql).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                let e = QueryError::from(e);
                if tries < max && retry::is_retryable(e.sqlstate.as_deref(), false, false) {
                    tokio::time::sleep(retry::backoff_delay(tries, retry::jitter_from_clock()))
                        .await;
                    tries += 1;
                } else {
                    return Err(e);
                }
            }
        }
    }
}

/// Run `statements` in one explicit transaction. On failure the transaction is
/// rolled back, and the connection is closed if even that fails.
async fn run_in_transaction(client: Object, statements: &[String]) -> Result<(), QueryError> {
    let script = format!("BEGIN; {}; COMMIT", statements.join("; "));
    match client.batch_execute(&script).await {
        Ok(()) => Ok(()),
        Err(e) => {
            if client.batch_execute("ROLLBACK").await.is_err() {
                drop(Object::take(client));
            }
            Err(e.into())
        }
    }
}

async fn checkout(
    conn: &ConnectionParams,
    params: &Value,
) -> Result<(Object, String), PluginError> {
    let entry = client::get_pool(conn, PoolKind::Query)?;
    let client = entry.pool.get().await.map_err(client::pool_error)?;
    let schema = match optional(params, "schema") {
        Some(s) => s.to_string(),
        None => entry
            .server_info()
            .map(|i| i.current_schema)
            .unwrap_or_else(|| "public".to_string()),
    };
    Ok((client, schema))
}

pub async fn drop_index(id: Value, params: &Value) -> Value {
    result_response(id, drop_index_inner(params).await)
}

async fn drop_index_inner(params: &Value) -> Result<Value, PluginError> {
    let table = text(params, &["table"])?;
    let index = text(params, &["index_name"])?;
    let (client, schema) = checkout(&connection(params), params).await?;
    let sql = ddl::build_drop_index(&schema, table, index)?;
    match run_statement(&client, &sql).await {
        Ok(()) => Ok(Value::Null),
        // An index behind a UNIQUE constraint that other tables reference. CASCADE would
        // drop those foreign keys too, which is not something to do without being asked.
        Err(e) if e.sqlstate.as_deref() == Some("2BP01") => Err(PluginError::internal(format!(
            "{}\nNothing was dropped. To drop it together with everything that depends on it, \
             run `{sql} CASCADE` in the SQL editor (this also removes the foreign keys that use it).",
            e.message
        ))),
        Err(e) => Err(query_error(e)),
    }
}

pub async fn drop_foreign_key(id: Value, params: &Value) -> Value {
    result_response(id, drop_foreign_key_inner(params).await)
}

async fn drop_foreign_key_inner(params: &Value) -> Result<Value, PluginError> {
    let table = text(params, &["table"])?;
    let fk = text(params, &["fk_name", "constraint_name"])?;
    let (client, schema) = checkout(&connection(params), params).await?;
    let sql = ddl::build_drop_foreign_key(&schema, table, fk)?;
    run_statement(&client, &sql).await.map_err(query_error)?;
    Ok(Value::Null)
}

pub async fn create_view(id: Value, params: &Value) -> Value {
    result_response(id, create_view_inner(params).await)
}

async fn create_view_inner(params: &Value) -> Result<Value, PluginError> {
    let view = text(params, &["view_name", "name"])?;
    let definition = text(params, &["definition"])?;
    let (client, schema) = checkout(&connection(params), params).await?;
    let sql = ddl::build_create_view(&schema, view, definition)?;
    run_statement(&client, &sql).await.map_err(query_error)?;
    Ok(Value::Null)
}

/// Replace a view's query. `CREATE OR REPLACE VIEW` can append columns but not
/// change or drop one (SQLSTATE `42P16`). In that case the view is dropped and
/// created again in one transaction: if anything fails between the two, the old
/// view comes back. The view is still not visible for a short moment (about
/// 70 ms measured on v26.3.3) while CockroachDB applies the change; a
/// transaction does not hide that. A view other objects depend on cannot be
/// dropped, and that error is returned as it is; nothing is cascaded.
pub async fn alter_view(id: Value, params: &Value) -> Value {
    result_response(id, alter_view_inner(params).await)
}

async fn alter_view_inner(params: &Value) -> Result<Value, PluginError> {
    let view = text(params, &["view_name", "name"])?;
    let definition = text(params, &["definition"])?;
    let (client, schema) = checkout(&connection(params), params).await?;
    let replace = ddl::build_replace_view(&schema, view, definition)?;
    match run_statement(&client, &replace).await {
        Ok(()) => Ok(Value::Null),
        Err(e) if e.sqlstate.as_deref() == Some(ddl::VIEW_REPLACE_INCOMPATIBLE) => {
            let drop = ddl::build_drop_view(&schema, view)?;
            let create = ddl::build_create_view(&schema, view, definition)?;
            run_in_transaction(client, &[drop, create])
                .await
                .map_err(query_error)?;
            Ok(Value::Null)
        }
        Err(e) => Err(query_error(e)),
    }
}

pub async fn drop_view(id: Value, params: &Value) -> Value {
    result_response(id, drop_view_inner(params).await)
}

async fn drop_view_inner(params: &Value) -> Result<Value, PluginError> {
    let view = text(params, &["view_name", "name"])?;
    let (client, schema) = checkout(&connection(params), params).await?;
    let sql = ddl::build_drop_view(&schema, view)?;
    run_statement(&client, &sql).await.map_err(query_error)?;
    Ok(Value::Null)
}

#[cfg(test)]
#[path = "ddl_tests.rs"]
mod tests;
