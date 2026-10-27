//! Row editing: `insert_record`, `update_record`, `delete_record`.
//!
//! Contract (read from the host, which differs from the plugin guide):
//! requests carry `pk_map` (column -> value, so composite keys work) and
//! `max_blob_size`; the reply is the number of affected rows.
//!
//! Each write is its own driver-owned transaction:
//!
//! ```text
//! BEGIN READ WRITE; <statement>; COMMIT
//! ```
//!
//! `READ WRITE` keeps edits working on a connection or role whose default is
//! read-only (a plain write fails there with SQLSTATE 25006). On a
//! serialization failure (`40001`), at the statement or at `COMMIT`, the whole
//! transaction is rolled back and run again, up to `retryMaxAttempts`.
//! Restarting the transaction is equivalent to the documented
//! `SAVEPOINT cockroach_restart` protocol for a transaction of one statement,
//! and also covers a failure at `COMMIT`, which the savepoint cannot.
//!
//! The host sends no `session_id` with these calls, so there is never an open
//! user transaction to join.

use deadpool_postgres::Object;
use serde_json::{json, Map, Value};

use crate::client::{self, PoolKind};
use crate::crud::{self, ColumnInfo, Columns, Statement};
use crate::error::PluginError;
use crate::handlers::query::QueryError;
use crate::models::{inner_params, ConnectionParams};
use crate::retry;
use crate::rpc::result_response;
use crate::settings;

/// The request fields every CRUD call shares.
pub struct Target {
    pub conn: ConnectionParams,
    pub schema: Option<String>,
    pub table: String,
}

impl Target {
    pub fn from_params(params: &Value) -> Result<Self, PluginError> {
        let table = params
            .get("table")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| PluginError::invalid_params("missing parameter 'table'"))?
            .to_string();
        Ok(Self {
            conn: ConnectionParams::from_value(inner_params(params)),
            schema: params
                .get("schema")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            table,
        })
    }
}

pub fn pk_map_param(params: &Value) -> Result<Map<String, Value>, PluginError> {
    match params.get("pk_map") {
        Some(Value::Object(m)) if !m.is_empty() => Ok(m.clone()),
        _ => Err(PluginError::invalid_params(
            "pk_map is required and must not be empty: refusing to match every row",
        )),
    }
}

fn max_blob_size(params: &Value) -> Option<u64> {
    params
        .get("max_blob_size")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
}

/// A connection plus the resolved schema and the table's columns.
pub struct Prepared {
    pub client: Object,
    pub schema: String,
    pub columns: Columns,
}

/// Check out a connection and read the table's real column types. Hidden
/// columns (`rowid`) are included, because a table without a primary key is
/// keyed by it. An unknown table is reported here, before anything is bound.
pub async fn prepare(target: &Target) -> Result<Prepared, PluginError> {
    let entry = client::get_pool(&target.conn, PoolKind::Query)?;
    let client = entry.pool.get().await.map_err(client::pool_error)?;
    let schema = match &target.schema {
        Some(s) => s.clone(),
        None => entry
            .server_info()
            .map(|i| i.current_schema)
            .unwrap_or_else(|| "public".to_string()),
    };
    let rows = client
        .query(
            "SELECT column_name::STRING, crdb_sql_type::STRING, data_type::STRING, \
                    udt_schema::STRING, udt_name::STRING \
             FROM information_schema.columns WHERE table_schema = $1 AND table_name = $2",
            &[&schema, &target.table],
        )
        .await?;
    if rows.is_empty() {
        return Err(PluginError::invalid_params(format!(
            "table \"{schema}\".\"{}\" was not found (or you have no access to it)",
            target.table
        )));
    }
    let columns = rows
        .iter()
        .map(|r| {
            let (name, sql_type, data_type, udt_schema, udt_name): (
                String,
                String,
                String,
                String,
                String,
            ) = (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4));
            let enum_type = data_type
                .eq_ignore_ascii_case("USER-DEFINED")
                .then(|| crate::binding::quote_enum_type(&udt_schema, &udt_name));
            (
                name,
                ColumnInfo {
                    sql_type,
                    enum_type,
                },
            )
        })
        .collect();
    Ok(Prepared {
        client,
        schema,
        columns,
    })
}

/// Roll back, reporting whether the connection can still be trusted.
async fn rollback(client: &Object) -> bool {
    client.batch_execute("ROLLBACK").await.is_ok()
}

/// One attempt: `BEGIN READ WRITE`, the statement, then `COMMIT`.
/// `single_row` makes more than one affected row an error that rolls back.
async fn attempt(client: &Object, stmt: &Statement, single_row: bool) -> Result<u64, QueryError> {
    client.batch_execute("BEGIN READ WRITE").await?;
    let outcome: Result<u64, QueryError> = async {
        let prepared = client.prepare_typed(&stmt.sql, &stmt.types).await?;
        Ok(client.execute(&prepared, &stmt.param_refs()).await?)
    }
    .await;
    match outcome {
        Ok(affected) if single_row && affected > 1 => {
            let _ = rollback(client).await;
            Err(QueryError {
                message: format!(
                    "refusing to change {affected} rows: the row key matches more than one row, \
                     so it does not identify a single row. Nothing was changed."
                ),
                sqlstate: None,
            })
        }
        Ok(affected) => match client.batch_execute("COMMIT").await {
            Ok(()) => Ok(affected),
            Err(e) => {
                let _ = rollback(client).await;
                Err(e.into())
            }
        },
        Err(e) => {
            let _ = rollback(client).await;
            Err(e)
        }
    }
}

/// Run a write as a driver-owned transaction, retrying `40001`. The
/// connection is closed instead of returned to the pool if it cannot be
/// brought back to a clean state.
pub async fn run_write(
    client: Object,
    stmt: &Statement,
    single_row: bool,
) -> Result<u64, QueryError> {
    let max_attempts = settings::get().retry_max_attempts;
    let mut tries = 0;
    let result = loop {
        match attempt(&client, stmt, single_row).await {
            Err(e)
                if tries < max_attempts
                    && retry::is_retryable(e.sqlstate.as_deref(), false, false) =>
            {
                tokio::time::sleep(retry::backoff_delay(tries, retry::jitter_from_clock())).await;
                tries += 1;
            }
            other => break other,
        }
    };
    // A failed run must not leave a transaction behind for the next borrower.
    if result.is_err() && !rollback(&client).await {
        drop(Object::take(client));
    }
    result
}

fn query_error(e: QueryError) -> PluginError {
    PluginError::internal(e.message)
}

pub async fn insert_record(id: Value, params: &Value) -> Value {
    result_response(id, insert(params).await.map(|n| json!(n)))
}

async fn insert(params: &Value) -> Result<u64, PluginError> {
    let target = Target::from_params(params)?;
    let data = match params.get("data") {
        Some(Value::Object(m)) => m.clone(),
        None | Some(Value::Null) => Map::new(),
        _ => {
            return Err(PluginError::invalid_params(
                "'data' must be an object of column values",
            ))
        }
    };
    let prep = prepare(&target).await?;
    let stmt = crud::build_insert(
        &prep.schema,
        &target.table,
        &data,
        &prep.columns,
        max_blob_size(params),
    )?;
    run_write(prep.client, &stmt, false)
        .await
        .map_err(query_error)
}

pub async fn update_record(id: Value, params: &Value) -> Value {
    result_response(id, update(params).await.map(|n| json!(n)))
}

async fn update(params: &Value) -> Result<u64, PluginError> {
    let target = Target::from_params(params)?;
    let col_name = params
        .get("col_name")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .ok_or_else(|| PluginError::invalid_params("missing parameter 'col_name'"))?;
    let new_val = params.get("new_val").cloned().unwrap_or(Value::Null);
    let pk_map = pk_map_param(params)?;
    let prep = prepare(&target).await?;
    let stmt = crud::build_update(
        &prep.schema,
        &target.table,
        &pk_map,
        col_name,
        new_val,
        &prep.columns,
        max_blob_size(params),
    )?;
    let affected = run_write(prep.client, &stmt, true)
        .await
        .map_err(query_error)?;
    if affected == 0 {
        // Not success: the row was deleted or its key changed since it was read.
        return Err(PluginError::internal(
            "row not found; it may have been changed or deleted (0 rows matched)",
        ));
    }
    Ok(affected)
}

pub async fn delete_record(id: Value, params: &Value) -> Value {
    result_response(id, delete(params).await.map(|n| json!(n)))
}

async fn delete(params: &Value) -> Result<u64, PluginError> {
    let target = Target::from_params(params)?;
    let pk_map = pk_map_param(params)?;
    let prep = prepare(&target).await?;
    let stmt = crud::build_delete(&prep.schema, &target.table, &pk_map, &prep.columns)?;
    run_write(prep.client, &stmt, true)
        .await
        .map_err(query_error)
}
