//! Live tests: drive the plugin binary over JSON-RPC against a real node.
//! Set `COCKROACH_HOST` (and optionally `COCKROACH_PORT`) to enable; the node
//! is assumed to be started with `--insecure`.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

fn rpc(method: &str, params: Value) -> Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_cockroachdb-plugin"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let req = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    writeln!(child.stdin.take().unwrap(), "{req}").unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let _ = child.wait();
    serde_json::from_str(&line).unwrap()
}

fn conn(ssl_mode: &str) -> Option<Value> {
    let host = std::env::var("COCKROACH_HOST").ok()?;
    let port: u16 = std::env::var("COCKROACH_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(26257);
    Some(
        json!({"params": {"driver": "cockroachdb", "host": host, "port": port,
        "username": "root", "ssl_mode": ssl_mode}}),
    )
}

#[test]
fn test_connection_reports_version() {
    let Some(p) = conn("disable") else { return };
    let r = rpc("test_connection", p);
    assert_eq!(r["result"]["success"], true, "{r}");
    assert!(r["result"]["version"]
        .as_str()
        .unwrap()
        .starts_with("CockroachDB"));
}

#[test]
fn prefer_falls_back_on_insecure_cluster() {
    let Some(p) = conn("prefer") else { return };
    assert_eq!(rpc("test_connection", p)["result"]["success"], true);
}

#[test]
fn ping_succeeds() {
    let Some(p) = conn("disable") else { return };
    let r = rpc("ping", p);
    assert!(r.get("error").is_none(), "{r}");
}

#[test]
fn refused_connection_gives_readable_error() {
    let p = json!({"params": {"host": "127.0.0.1", "port": 1, "ssl_mode": "disable"}});
    let r = rpc("test_connection", p);
    assert!(
        r["error"]["message"].as_str().unwrap().contains("refused"),
        "{r}"
    );
}
