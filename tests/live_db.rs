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

// ---- Phase 2: introspection -------------------------------------------

use std::sync::Once;

const SCHEMA: &str = "live_introspection";

fn seed() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let host = std::env::var("COCKROACH_HOST").unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (client, conn) = tokio_postgres::Config::new()
                .host(&host)
                .port(
                    std::env::var("COCKROACH_PORT")
                        .ok()
                        .and_then(|p| p.parse().ok())
                        .unwrap_or(26257),
                )
                .user("root")
                .dbname("defaultdb")
                .connect(tokio_postgres::NoTls)
                .await
                .unwrap();
            tokio::spawn(conn);
            client
                .batch_execute(&format!(
                    "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE;
                     CREATE SCHEMA {SCHEMA};
                     CREATE TYPE {SCHEMA}.mood AS ENUM ('sad','ok');
                     CREATE TABLE {SCHEMA}.users (id SERIAL PRIMARY KEY, email STRING NOT NULL,
                        name VARCHAR(50), price DECIMAL(10,2), m {SCHEMA}.mood,
                        INDEX idx_name (name) STORING (price), UNIQUE INDEX idx_email (email));
                     COMMENT ON TABLE {SCHEMA}.users IS 'User accounts';
                     COMMENT ON COLUMN {SCHEMA}.users.email IS 'login email';
                     CREATE TABLE {SCHEMA}.pair (a INT, b INT, PRIMARY KEY (a, b));
                     CREATE TABLE {SCHEMA}.nopk (a INT, b INT, INDEX idx_name (a));
                     CREATE TABLE {SCHEMA}.child (id INT PRIMARY KEY, a INT, b INT,
                        FOREIGN KEY (a, b) REFERENCES {SCHEMA}.pair (a, b) ON DELETE CASCADE);
                     CREATE VIEW {SCHEMA}.v_users AS SELECT id, email FROM {SCHEMA}.users;"
                ))
                .await
                .unwrap();
        });
    });
}

fn call(method: &str, extra: Value) -> Option<Value> {
    let mut p = conn("disable")?;
    p["params"]["database"] = json!("defaultdb");
    p["schema"] = json!(SCHEMA);
    for (k, v) in extra.as_object().unwrap() {
        p[k] = v.clone();
    }
    seed();
    let r = rpc(method, p);
    assert!(r.get("error").is_none(), "{method}: {r}");
    Some(r["result"].clone())
}

fn names(v: &Value, key: &str) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x[key].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn lists_databases_and_hides_system() {
    let Some(r) = call("get_databases", json!({})) else {
        return;
    };
    let dbs: Vec<_> = r
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d.as_str().unwrap())
        .collect();
    assert!(dbs.contains(&"defaultdb") && dbs.contains(&"postgres"));
    assert!(!dbs.contains(&"system"));
}

#[test]
fn lists_schemas_without_system_schemas() {
    let Some(r) = call("get_schemas", json!({})) else {
        return;
    };
    let s: Vec<_> = r
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d.as_str().unwrap())
        .collect();
    assert!(s.contains(&"public") && s.contains(&SCHEMA));
    assert!(
        !s.contains(&"crdb_internal")
            && !s.contains(&"pg_catalog")
            && !s.contains(&"information_schema")
    );
}

#[test]
fn tables_use_requested_schema_and_real_comments_only() {
    let Some(r) = call("get_tables", json!({})) else {
        return;
    };
    let tables = r.as_array().unwrap();
    assert_eq!(names(&r, "name"), ["child", "nopk", "pair", "users"]);
    let comment = |n: &str| tables.iter().find(|t| t["name"] == n).unwrap()["comment"].clone();
    assert_eq!(comment("users"), "User accounts");
    // pg_description also holds built-in function comments with colliding OIDs.
    assert!(comment("child").is_null() && comment("pair").is_null());
    assert!(tables.iter().all(|t| t["schema"] == SCHEMA));
}

#[test]
fn primary_keys_do_not_leak_across_tables() {
    let pk = |t: &str, col: &str| {
        let r = call("get_columns", json!({"table": t})).unwrap();
        r.as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == col)
            .unwrap()["is_pk"]
            .clone()
    };
    if conn("disable").is_none() {
        return;
    }
    // `pair` has PRIMARY KEY (a, b); `nopk` has same-named, non-key columns.
    assert_eq!(pk("pair", "a"), true);
    assert_eq!(pk("nopk", "a"), false);
    assert_eq!(pk("nopk", "b"), false);
}

#[test]
fn columns_report_types_comments_and_hide_rowid() {
    let Some(r) = call("get_columns", json!({"table": "users"})) else {
        return;
    };
    let cols = r.as_array().unwrap();
    let col = |n: &str| cols.iter().find(|c| c["name"] == n).unwrap().clone();
    assert_eq!(col("id")["data_type"], "INT8");
    assert_eq!(col("id")["is_auto_increment"], true);
    assert_eq!(col("name")["data_type"], "VARCHAR");
    assert_eq!(col("name")["character_maximum_length"], 50);
    assert_eq!(col("price")["numeric_precision"], 10);
    assert_eq!(col("email")["comment"], "login email");
    assert_eq!(col("m")["data_type"], "ENUM(mood)");
    let nopk = call("get_columns", json!({"table": "nopk"})).unwrap();
    assert_eq!(names(&nopk, "name"), ["a", "b"]);
}

#[test]
fn composite_foreign_key_pairs_columns_by_position() {
    let Some(r) = call("get_foreign_keys", json!({"table": "child"})) else {
        return;
    };
    let fk = r.as_array().unwrap();
    assert_eq!(fk.len(), 2);
    assert_eq!(
        (fk[0]["column_name"].as_str(), fk[0]["ref_column"].as_str()),
        (Some("a"), Some("a"))
    );
    assert_eq!(
        (fk[1]["column_name"].as_str(), fk[1]["ref_column"].as_str()),
        (Some("b"), Some("b"))
    );
    assert_eq!(fk[0]["name"], fk[1]["name"]);
    assert_eq!(
        (fk[0]["seq_in_fk"].as_i64(), fk[1]["seq_in_fk"].as_i64()),
        (Some(1), Some(2))
    );
    assert_eq!(fk[0]["on_delete"], "CASCADE");
    assert_eq!(fk[0]["ref_table"], "pair");
}

#[test]
fn indexes_have_one_entry_per_key_column_in_the_host_shape() {
    let Some(r) = call("get_indexes", json!({"table": "users"})) else {
        return;
    };
    let entries = r.as_array().unwrap();
    for e in entries {
        // Fields the host requires; a missing one blanks the whole table.
        assert!(e["name"].is_string() && e["column_name"].is_string());
        assert!(e["is_unique"].is_boolean() && e["is_primary"].is_boolean());
        assert!(e["seq_in_index"].is_i64());
    }
    let of = |n: &str| -> Vec<&Value> { entries.iter().filter(|e| e["name"] == n).collect() };
    assert_eq!(of("users_pkey")[0]["is_primary"], true);
    assert_eq!(of("users_pkey")[0]["column_name"], "id");
    // STORING columns are not key columns.
    assert_eq!(of("idx_name").len(), 1);
    assert_eq!(of("idx_name")[0]["column_name"], "name");
    assert_eq!(of("idx_email")[0]["is_unique"], true);
    assert_eq!(of("idx_email")[0]["is_primary"], false);
    // Same index name on another table is independent.
    let other = call("get_indexes", json!({"table": "nopk"})).unwrap();
    let n = other
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["name"] == "idx_name")
        .unwrap();
    assert_eq!(n["column_name"], "a");
}

#[test]
fn batch_methods_key_results_by_table() {
    let Some(cols) = call("get_all_columns_batch", json!({"tables": ["pair", "nopk"]})) else {
        return;
    };
    assert_eq!(cols["pair"].as_array().unwrap().len(), 2);
    assert_eq!(cols["nopk"][0]["is_pk"], false);
    let fks = call(
        "get_all_foreign_keys_batch",
        json!({"tables": ["child", "pair"]}),
    )
    .unwrap();
    assert_eq!(fks["child"].as_array().unwrap().len(), 2);
    assert_eq!(fks["pair"].as_array().unwrap().len(), 0);
}

#[test]
fn views_and_ddl() {
    let Some(v) = call("get_views", json!({})) else {
        return;
    };
    assert_eq!(names(&v, "name"), ["v_users"]);
    let def = call("get_view_definition", json!({"view": "v_users"})).unwrap();
    assert!(def.as_str().unwrap().to_uppercase().starts_with("SELECT"));
    let cols = call("get_view_columns", json!({"view": "v_users"})).unwrap();
    assert_eq!(names(&cols, "name"), ["id", "email"]);
    let ddl = call("get_table_ddl", json!({"table": "users"})).unwrap();
    assert!(ddl.as_str().unwrap().contains("CREATE TABLE"));
}
