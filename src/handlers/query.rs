//! Connection and query execution.
//!
use serde_json::{json, Value};

use crate::client;
use crate::models::{inner_params, ConnectionParams};
use crate::rpc::{error_response, not_implemented, ok_response};

pub fn test_connection(id: Value, params: &Value) -> Value {
    let conn = ConnectionParams::from_value(inner_params(params));
    let schema = params.get("schema").and_then(Value::as_str);
    match client::block_on(client::test_connection(&conn, schema)) {
        Ok(info) => ok_response(
            id,
            json!({ "success": true, "version": info.version_string }),
        ),
        Err(e) => error_response(id, e.code, &e.message),
    }
}

pub fn ping(id: Value, params: &Value) -> Value {
    let conn = ConnectionParams::from_value(inner_params(params));
    match client::block_on(client::ping(&conn)) {
        Ok(()) => ok_response(id, Value::Null),
        Err(e) => error_response(id, e.code, &e.message),
    }
}

pub fn execute_query(id: Value, _params: &Value) -> Value {
    // TODO: run the SQL in params.query and return
    //   { columns: [string], rows: [[any]], total_count: number, execution_time_ms: number }
    not_implemented(id, "execute_query")
}

pub fn explain_query(id: Value, _params: &Value) -> Value {
    // TODO: return the same shape as execute_query but for EXPLAIN.
    not_implemented(id, "explain_query")
}
