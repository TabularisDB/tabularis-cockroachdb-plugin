use serde_json::json;

use super::*;

fn conn(v: serde_json::Value) -> Connection {
    Connection::resolve(&ConnectionParams::from_value(&v), &Settings::default()).unwrap()
}

#[test]
fn applies_defaults() {
    let c = conn(json!({"host": "h"}));
    assert_eq!(
        (c.port, c.database.as_str(), c.user.as_str()),
        (26257, "defaultdb", "root")
    );
    assert_eq!(c.ssl_mode, SslMode::Prefer);
    assert_eq!(c.application_name, "tabularis");
}

#[test]
fn requires_host() {
    assert!(Connection::resolve(
        &ConnectionParams::from_value(&json!({})),
        &Settings::default()
    )
    .is_err());
}

#[test]
fn uri_fills_gaps_and_overrides_tls() {
    let c = conn(json!({
        "host": "form-host", "ssl_mode": "require", "ssl_ca": "/form-ca",
        "connection_uri": "cockroachdb://u:p@uri-host:1234/db?sslmode=verify-full&sslrootcert=/uri-ca&options=--cluster%3Dabc&application_name=app"
    }));
    assert_eq!(c.host, "form-host");
    assert_eq!(c.port, 1234);
    assert_eq!(c.database, "db");
    assert_eq!((c.user.as_str(), c.password.as_str()), ("u", "p"));
    assert_eq!(c.ssl_mode, SslMode::VerifyFull);
    assert_eq!(c.ssl_ca.as_deref(), Some("/uri-ca"));
    assert_eq!(c.options.as_deref(), Some("--cluster=abc"));
    assert_eq!(c.application_name, "app");
}

#[test]
fn extra_cluster_becomes_options() {
    let c = conn(json!({"host": "h", "extra": {"cluster": "my-cluster-1"}}));
    assert_eq!(c.options.as_deref(), Some("--cluster=my-cluster-1"));
}

#[test]
fn pool_key_covers_tls_options_and_kind() {
    let base = conn(json!({"host": "h", "ssl_mode": "verify-full", "ssl_ca": "/a"}));
    let k = |c: &Connection| c.pool_key(PoolKind::Query, 10);
    let mut other = base.clone();
    other.ssl_ca = Some("/b".into());
    assert_ne!(k(&base), k(&other));
    let mut other = base.clone();
    other.ssl_cert = Some("/c".into());
    assert_ne!(k(&base), k(&other));
    let mut other = base.clone();
    other.options = Some("--cluster=x".into());
    assert_ne!(k(&base), k(&other));
    let mut other = base.clone();
    other.password = "pw".into();
    assert_ne!(k(&base), k(&other));
    let mut other = base.clone();
    other.ssl_mode = SslMode::Require;
    assert_ne!(k(&base), k(&other));
    assert_ne!(
        base.pool_key(PoolKind::Query, 10),
        base.pool_key(PoolKind::Metadata, 10)
    );
    assert_eq!(k(&base), k(&base.clone()));
}

#[test]
fn pg_config_carries_routing_and_app_name() {
    let c = conn(json!({"host": "h", "extra": {"cluster": "c1"}}));
    let cfg = c.pg_config(PoolKind::Metadata);
    assert_eq!(cfg.get_options(), Some("--cluster=c1"));
    assert_eq!(cfg.get_application_name(), Some("tabularis metadata"));
}
