//! Registry of in-flight statements, keyed by JSON-RPC request id, so the
//! host's `cancel` notification can stop the one it names.
//!
//! The query handlers register a cancel action before running a statement
//! and a [`CancelGuard`] removes it when the statement finishes (success,
//! error, or early return), so the map cannot grow and a late cancel can
//! never hit a later statement that reused an id.
//!
//! The mechanism is `tokio-postgres`'s `CancelToken::cancel_query`, which
//! sends a pgwire cancel request on a new TCP connection. CockroachDB honours
//! it on the supported releases. Behind a load balancer the request may reach
//! another node and be ignored; the documented fallback
//! (`CANCEL QUERY ... SHOW CLUSTER STATEMENTS`) is not implemented yet.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{LazyLock, Mutex};

use deadpool_postgres::Object;
use serde_json::Value;

use crate::client::Connection;

type CancelAction = Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

static HANDLES: LazyLock<Mutex<HashMap<String, CancelAction>>> = LazyLock::new(Default::default);

/// Map key for a JSON-RPC id (numbers and strings both occur).
pub fn key_for(id: &Value) -> Option<String> {
    match id {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

fn insert(key: String, action: CancelAction) {
    if let Ok(mut h) = HANDLES.lock() {
        h.insert(key, action);
    }
}

fn remove(key: &str) {
    if let Ok(mut h) = HANDLES.lock() {
        h.remove(key);
    }
}

/// Deregisters on drop. A guard without a key does nothing.
pub struct CancelGuard {
    key: Option<String>,
}

impl CancelGuard {
    /// Register a cancel action for `client`'s backend under `request_id`.
    pub fn register(request_id: &Value, client: &Object, conn: Connection) -> Self {
        let key = key_for(request_id);
        if let Some(k) = &key {
            let token = client.cancel_token();
            insert(
                k.clone(),
                Box::new(move || {
                    Box::pin(async move {
                        let tls = match conn.connector() {
                            Ok(t) => t,
                            Err(e) => {
                                eprintln!("cancel: cannot build TLS connector: {}", e.message);
                                return;
                            }
                        };
                        if let Err(e) = token.cancel_query(tls).await {
                            eprintln!(
                                "cancel: request failed: {}",
                                crate::error::describe_pg_error(&e)
                            );
                        }
                    })
                }),
            );
        }
        Self { key }
    }

    /// A guard around an arbitrary action (used by tests).
    #[cfg(test)]
    fn with_action(request_id: &Value, action: CancelAction) -> Self {
        let key = key_for(request_id);
        if let Some(k) = &key {
            insert(k.clone(), action);
        }
        Self { key }
    }
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        if let Some(k) = &self.key {
            remove(k);
        }
    }
}

/// Run and consume the action registered for `request_id`. An unknown id
/// (already finished, never cancellable) is a no-op, never an error.
pub async fn cancel(request_id: &Value) {
    let action =
        key_for(request_id).and_then(|k| HANDLES.lock().ok().and_then(|mut h| h.remove(&k)));
    if let Some(action) = action {
        action().await;
    }
}

#[cfg(test)]
#[path = "cancel_tests.rs"]
mod tests;
