//! JSON-RPC dispatch and response helpers.

use serde_json::{json, Value};

use crate::handlers;

/// The `method` of a request line, without handling it. Used to route a line
/// before a worker picks it up. `None` for lines that are not a JSON object
/// with a string method; those are answered with an error by [`handle_line`].
pub fn peek_method(line: &str) -> Option<String> {
    serde_json::from_str::<Value>(line)
        .ok()?
        .get("method")?
        .as_str()
        .map(str::to_string)
}

/// Parse one JSON-RPC line and return the response value (serialised
/// downstream by `main.rs`). Never panics: parse errors and method failures
/// are surfaced as JSON-RPC error responses.
///
/// Returns `None` for a notification (a request with no `id`), such as the
/// host's `cancel`. A notification gets no reply; writing one would put an
/// unexpected line on stdout.
pub async fn handle_line(line: &str) -> Option<Value> {
    let request: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(err) => {
            return Some(error_response(
                Value::Null,
                -32700,
                &format!("parse error: {err}"),
            ))
        }
    };

    let is_notification = request.get("id").is_none();
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let params = request.get("params").cloned().unwrap_or(Value::Null);

    let response = dispatch(&method, id, params).await;
    if is_notification {
        None
    } else {
        Some(response)
    }
}

async fn dispatch(method: &str, id: Value, params: Value) -> Value {
    match method {
        "initialize" => {
            crate::settings::set(crate::settings::Settings::from_value(&params));
            ok_response(id, Value::Null)
        }
        "shutdown" => {
            crate::session::release_all().await;
            ok_response(id, Value::Null)
        }
        "ping" => handlers::query::ping(id, &params).await,
        "test_connection" => handlers::query::test_connection(id, &params).await,

        // Metadata — return empty arrays so the driver loads cleanly.
        "get_databases" => handlers::metadata::get_databases(id, &params).await,
        "get_schemas" => handlers::metadata::get_schemas(id, &params).await,
        "get_tables" => handlers::metadata::get_tables(id, &params).await,
        "get_columns" => handlers::metadata::get_columns(id, &params).await,
        "get_foreign_keys" => handlers::metadata::get_foreign_keys(id, &params).await,
        "get_indexes" => handlers::metadata::get_indexes(id, &params).await,
        "get_views" => handlers::metadata::get_views(id, &params).await,
        "get_view_definition" => handlers::metadata::get_view_definition(id, &params).await,
        "get_view_columns" => handlers::metadata::get_view_columns(id, &params).await,
        "get_routines" => handlers::metadata::get_routines(id, &params).await,
        "get_routine_parameters" => handlers::metadata::get_routine_parameters(id, &params).await,
        "get_routine_definition" => handlers::metadata::get_routine_definition(id, &params).await,
        "get_schema_snapshot" => handlers::metadata::get_schema_snapshot(id, &params).await,
        "get_all_columns_batch" => handlers::metadata::get_all_columns_batch(id, &params).await,
        "get_all_foreign_keys_batch" => {
            handlers::metadata::get_all_foreign_keys_batch(id, &params).await
        }

        "get_table_ddl" => handlers::metadata::get_table_ddl(id, &params).await,

        // View mutation — not implemented by default.
        "create_view" | "alter_view" | "drop_view" => not_implemented(id, method),

        // Query execution — critical but needs a real driver.
        "execute_query" => handlers::query::execute_query(id, &params).await,
        "execute_query_batch" => handlers::query::execute_query_batch(id, &params).await,
        "release_session" => handlers::query::release_session(id, &params).await,
        "cancel" => handlers::query::cancel(id, &params).await,
        "explain_query" => handlers::query::explain_query(id, &params).await,

        // CRUD.
        "insert_record" => handlers::crud::insert_record(id, &params),
        "update_record" => handlers::crud::update_record(id, &params),
        "delete_record" => handlers::crud::delete_record(id, &params),

        // DDL.
        "get_create_table_sql" => handlers::ddl::get_create_table_sql(id, &params),
        "get_add_column_sql" => handlers::ddl::get_add_column_sql(id, &params),
        "get_alter_column_sql" => handlers::ddl::get_alter_column_sql(id, &params),
        "get_create_index_sql" => handlers::ddl::get_create_index_sql(id, &params),
        "get_create_foreign_key_sql" => handlers::ddl::get_create_foreign_key_sql(id, &params),
        "drop_index" => handlers::ddl::drop_index(id, &params),
        "drop_foreign_key" => handlers::ddl::drop_foreign_key(id, &params),

        other => not_implemented(id, other),
    }
}

pub fn ok_response(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "result": result,
        "id": id,
    })
}

/// Wrap a handler result: `Ok` becomes a result, `Err` a JSON-RPC error.
pub fn result_response(id: Value, result: Result<Value, crate::error::PluginError>) -> Value {
    match result {
        Ok(v) => ok_response(id, v),
        Err(e) => error_response(id, e.code, &e.message),
    }
}

pub fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "error": { "code": code, "message": message },
        "id": id,
    })
}

/// Keep code -32601 and "Method not found" in the message: most host fallbacks
/// for optional methods (batch execution, materialized views, BLOBs, ...)
/// match on the message text, a few on the code.
pub fn not_implemented(id: Value, method: &str) -> Value {
    error_response(id, -32601, &format!("Method not found: {method}"))
}
