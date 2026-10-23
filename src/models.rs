//! Shared request/response shapes.
//!
//! These mirror the `ConnectionParams` struct the host sends. Keep fields
//! optional — different database types leave different fields blank.

use serde_json::Value;

#[derive(Debug, Clone)]
pub struct ConnectionParams {
    pub driver: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub database: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub ssl_mode: Option<String>,
    pub ssl_ca: Option<String>,
    pub ssl_cert: Option<String>,
    pub ssl_key: Option<String>,
    /// Raw URI when the user imported a connection string.
    pub connection_uri: Option<String>,
    /// Plugin-owned fields (e.g. `cluster`).
    pub extra: Value,
}

impl ConnectionParams {
    pub fn from_value(value: &Value) -> Self {
        let obj = value.as_object();
        let get_str = |k: &str| {
            obj.and_then(|o| o.get(k))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let port = obj
            .and_then(|o| o.get("port"))
            .and_then(Value::as_u64)
            .and_then(|p| u16::try_from(p).ok());

        Self {
            driver: get_str("driver"),
            host: get_str("host"),
            port,
            database: get_str("database"),
            username: get_str("username"),
            password: get_str("password"),
            ssl_mode: get_str("ssl_mode"),
            ssl_ca: get_str("ssl_ca"),
            ssl_cert: get_str("ssl_cert"),
            ssl_key: get_str("ssl_key"),
            connection_uri: get_str("connection_uri"),
            extra: obj
                .and_then(|o| o.get("extra"))
                .cloned()
                .unwrap_or(Value::Null),
        }
    }
}

/// Extract the nested `params` object every RPC method receives.
/// Tabularis wraps connection params in `params.params`.
pub fn inner_params(value: &Value) -> &Value {
    value.get("params").unwrap_or(&Value::Null)
}
