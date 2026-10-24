//! Plugin-local error type. Keep deps light — no `anyhow`/`thiserror` by default.

use std::fmt;

#[derive(Debug)]
pub struct PluginError {
    pub code: i64,
    pub message: String,
}

impl PluginError {
    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: msg.into(),
        }
    }

    pub fn invalid_params(msg: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: msg.into(),
        }
    }
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for PluginError {}

/// Turn raw driver/server text into a readable message. The original text is
/// kept after the explanation.
pub fn friendly_message(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    let hint = if lower.contains("node is running secure mode") {
        Some("The cluster requires TLS. Set SSL mode to require, verify-ca or verify-full (not disable)")
    } else if lower.contains("first record does not look like a tls handshake")
        || lower.contains("server does not support tls")
    {
        Some(
            "The server does not accept TLS (insecure cluster?). Set SSL mode to disable or prefer",
        )
    } else if lower.contains("codeparamsroutingfailed")
        || lower.contains("missing cluster identifier")
    {
        Some("CockroachDB Cloud needs a cluster routing option. Add options=--cluster%3D<routing-id> to the connection string")
    } else if lower.contains("password authentication failed") {
        Some("Authentication failed. Check the password; certificate-auth users need ssl_cert and ssl_key")
    } else if lower.contains("connection refused") {
        Some("Connection refused. Check host and port (CockroachDB listens on 26257, not 5432)")
    } else {
        None
    };
    match hint {
        Some(h) => format!("{h}. Original error: {raw}"),
        None => raw.to_string(),
    }
}

/// Describe a `tokio_postgres` error with SQLSTATE and the full cause chain.
pub fn describe_pg_error(err: &tokio_postgres::Error) -> String {
    if let Some(db) = err.as_db_error() {
        let code = db.code().code();
        if code == "57014" {
            return format!("Query cancelled (SQLSTATE {code})");
        }
        // Keep DETAIL and HINT: CockroachDB hints often link to the docs
        // (retry errors, unsupported features).
        let mut text = format!("{} (SQLSTATE {code})", db.message());
        if let Some(d) = db.detail().filter(|d| !d.is_empty()) {
            text.push_str(&format!("\nDETAIL: {d}"));
        }
        if let Some(h) = db.hint().filter(|h| !h.is_empty()) {
            text.push_str(&format!("\nHINT: {h}"));
        }
        return friendly_message(&text);
    }
    let mut text = err.to_string();
    let mut source = std::error::Error::source(err);
    while let Some(s) = source {
        text.push_str(": ");
        text.push_str(&s.to_string());
        source = s.source();
    }
    friendly_message(&text)
}

/// SQLSTATE of a server error, if the error came from the server.
pub fn pg_sqlstate(err: &tokio_postgres::Error) -> Option<String> {
    err.as_db_error().map(|d| d.code().code().to_string())
}

impl From<tokio_postgres::Error> for PluginError {
    fn from(err: tokio_postgres::Error) -> Self {
        PluginError::internal(describe_pg_error(&err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_failures() {
        let m = friendly_message("node is running secure mode, SSL connection required");
        assert!(m.contains("requires TLS") && m.contains("Original error: node is running"));
        assert!(
            friendly_message("tls: first record does not look like a TLS handshake")
                .contains("insecure")
        );
        assert!(friendly_message("missing cluster identifier").contains("--cluster"));
        assert!(friendly_message("Connection refused (os error 61)").contains("26257"));
        assert!(
            friendly_message("password authentication failed for user root").contains("ssl_cert")
        );
    }

    #[test]
    fn leaves_unknown_text_alone() {
        assert_eq!(
            friendly_message("relation does not exist"),
            "relation does not exist"
        );
    }
}
