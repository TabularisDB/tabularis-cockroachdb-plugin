//! Live tests: drive the plugin binary over JSON-RPC against a real node.
//!
//! Set `COCKROACH_HOST` (and `COCKROACH_PORT` if not 26257) to run them; the
//! node is assumed to be started with `--insecure`. Without a host they skip,
//! and a skipped test is reported as passed, so CI sets `REQUIRE_LIVE_DB=1`,
//! which makes a missing host a failure instead of a silent skip.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

/// The node to test against, or `None` to skip. Panics when the host is
/// missing and `REQUIRE_LIVE_DB` is set. (Same rule as `src/testenv.rs`; an
/// integration test cannot import from the binary crate.)
fn live_node() -> Option<(String, u16)> {
    let host = std::env::var("COCKROACH_HOST")
        .ok()
        .filter(|h| !h.is_empty());
    let required = std::env::var("REQUIRE_LIVE_DB").is_ok_and(|v| !v.is_empty() && v != "0");
    match host {
        Some(host) => {
            let port = std::env::var("COCKROACH_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(26257);
            Some((host, port))
        }
        None if required => {
            panic!("REQUIRE_LIVE_DB is set but COCKROACH_HOST is not: refusing to skip a live test")
        }
        None => None,
    }
}

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
    let (host, port) = live_node()?;
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
        let (host, port) = live_node().expect("live node");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (client, conn) = tokio_postgres::Config::new()
                .host(&host)
                .port(port)
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

// ---- Phase 3: query execution ------------------------------------------
//
// These talk to ONE long-lived plugin process, because pinned sessions and
// cancellation live inside it.

use std::process::{Child, ChildStdin};
use std::sync::mpsc;
use std::time::Duration;

struct Plugin {
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<String>,
    next_id: u64,
}

impl Plugin {
    fn start() -> Option<Self> {
        Self::start_with(json!({}))
    }

    /// Start with `initialize` settings (e.g. `{"poolMaxSize": 1}`).
    fn start_with(settings: Value) -> Option<Self> {
        let mut p = Self::spawn()?;
        let init = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"settings": settings}});
        writeln!(p.stdin, "{init}").unwrap();
        assert_eq!(p.recv(10).expect("initialize reply")["id"], 0);
        Some(p)
    }

    fn spawn() -> Option<Self> {
        live_node()?;
        let mut child = Command::new(env!("CARGO_BIN_EXE_cockroachdb-plugin"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Some(Self {
            child,
            stdin,
            lines,
            next_id: 1,
        })
    }

    fn conn(&self) -> Value {
        let mut p = conn("disable").unwrap();
        p["params"]["database"] = json!("defaultdb");
        p
    }

    /// Send a request without waiting; returns its id.
    fn send(&mut self, method: &str, extra: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let mut params = self.conn();
        for (k, v) in extra.as_object().unwrap() {
            params[k] = v.clone();
        }
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{req}").unwrap();
        id
    }

    fn recv(&self, secs: u64) -> Option<Value> {
        self.lines
            .recv_timeout(Duration::from_secs(secs))
            .ok()
            .map(|l| serde_json::from_str(&l).unwrap())
    }

    fn call(&mut self, method: &str, extra: Value) -> Value {
        let id = self.send(method, extra);
        let r = self.recv(20).expect("no response");
        assert_eq!(r["id"], id, "{r}");
        r
    }

    fn query(&mut self, sql: &str) -> Value {
        self.call("execute_query", json!({"query": sql}))
    }
}

impl Drop for Plugin {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[test]
fn query_decodes_values_and_keeps_big_ints_exact() {
    let Some(mut p) = Plugin::start() else { return };
    let r = p.query("SELECT 1152921504606846977::INT8 AS big, 42::INT8 AS small, 1.50::DECIMAL AS d, 'x' AS s, NULL::INT8 AS n");
    let res = &r["result"];
    assert_eq!(res["columns"], json!(["big", "small", "d", "s", "n"]));
    assert_eq!(
        res["rows"][0],
        json!(["1152921504606846977", 42, "1.50", "x", null])
    );
    assert_eq!(res["affected_rows"], 0);
}

#[test]
fn pagination_fetches_one_extra_row_to_report_has_more() {
    let Some(mut p) = Plugin::start() else { return };
    let page = |p: &mut Plugin, n: u64| {
        p.call("execute_query", json!({"query": "SELECT generate_series(1, 25) AS n ORDER BY 1", "limit": 10, "page": n}))["result"].clone()
    };
    let first = page(&mut p, 1);
    assert_eq!(first["rows"].as_array().unwrap().len(), 10);
    assert_eq!(first["pagination"]["has_more"], true);
    assert_eq!(first["truncated"], true);
    assert_eq!(first["rows"][0], json!([1]));
    let last = page(&mut p, 3);
    assert_eq!(last["rows"].as_array().unwrap().len(), 5);
    assert_eq!(last["pagination"]["has_more"], false);
    assert_eq!(last["rows"][0], json!([21]));
    // A user LIMIT caps the total across pages.
    let r = p.call("execute_query", json!({"query": "SELECT generate_series(1, 25) AS n ORDER BY 1 LIMIT 12", "limit": 10, "page": 2}));
    assert_eq!(r["result"]["rows"].as_array().unwrap().len(), 2);
}

#[test]
fn statements_that_reject_limit_are_capped_client_side() {
    let Some(mut p) = Plugin::start() else { return };
    let r = p.call("execute_query", json!({"query": "SHOW TABLES", "limit": 5}));
    assert!(r.get("error").is_none(), "{r}");
    assert!(r["result"]["pagination"].is_null());
}

#[test]
fn dml_reports_affected_rows_and_returning_rows() {
    let Some(mut p) = Plugin::start() else { return };
    p.query("DROP TABLE IF EXISTS live_q1");
    p.query("CREATE TABLE live_q1 (id INT8 PRIMARY KEY, v STRING)");
    let ins = p.query("INSERT INTO live_q1 VALUES (1,'a'),(2,'b'),(3,'c')");
    assert_eq!(ins["result"]["affected_rows"], 3, "{ins}");
    let upd = p.query("UPDATE live_q1 SET v = 'z' WHERE id < 3");
    assert_eq!(upd["result"]["affected_rows"], 2);
    let ret = p.query("INSERT INTO live_q1 VALUES (4,'d') RETURNING id, v");
    assert_eq!(ret["result"]["rows"], json!([[4, "d"]]), "{ret}");
    let del = p.query("DELETE FROM live_q1");
    assert_eq!(del["result"]["affected_rows"], 4);
    p.query("DROP TABLE live_q1");
}

#[test]
fn errors_carry_sqlstate_and_message() {
    let Some(mut p) = Plugin::start() else { return };
    let r = p.query("SELECT 1/0");
    let msg = r["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("division by zero") && msg.contains("SQLSTATE 22012"),
        "{msg}"
    );
    let r = p.query("SELECT * FROM table_that_does_not_exist");
    assert!(r["error"]["message"]
        .as_str()
        .unwrap()
        .contains("SQLSTATE 42P01"));
}

#[test]
fn batch_runs_in_order_and_reports_each_statement() {
    let Some(mut p) = Plugin::start() else { return };
    let r = p.call(
        "execute_query_batch",
        json!({"queries": ["SELECT 1", "SELECT 1/0", "SELECT 3"]}),
    );
    let results = r["result"]
        .as_array()
        .expect("bare array without a session_id");
    assert_eq!(results.len(), 3);
    assert_eq!(results[0]["result"]["rows"], json!([[1]]));
    assert!(
        results[1]["result"].is_null() && results[1]["error"].as_str().unwrap().contains("22012")
    );
    assert_eq!(
        results[2]["result"]["rows"],
        json!([[3]]),
        "later statements still run"
    );
    assert!(results[0]["execution_time_ms"].is_number());
}

#[test]
fn explicit_transaction_in_one_batch_commits_together() {
    let Some(mut p) = Plugin::start() else { return };
    p.query("DROP TABLE IF EXISTS live_q2");
    p.query("CREATE TABLE live_q2 (id INT8 PRIMARY KEY)");
    let r = p.call("execute_query_batch", json!({"queries": ["BEGIN", "INSERT INTO live_q2 VALUES (1)", "COMMIT"], "session_id": "t-batch"}));
    assert_eq!(r["result"]["in_transaction"], false, "{r}");
    let c = p.query("SELECT count(*) FROM live_q2");
    assert_eq!(c["result"]["rows"], json!([[1]]));
    p.query("DROP TABLE live_q2");
}

#[test]
fn session_keeps_a_transaction_open_across_runs() {
    let Some(mut p) = Plugin::start() else { return };
    p.query("DROP TABLE IF EXISTS live_q3");
    p.query("CREATE TABLE live_q3 (id INT8 PRIMARY KEY)");
    let q = |p: &mut Plugin, sql: &str, sid: Option<&str>| match sid {
        Some(s) => p.call("execute_query", json!({"query": sql, "session_id": s})),
        None => p.query(sql),
    };

    let begin = q(&mut p, "BEGIN", Some("tab-1"));
    assert_eq!(begin["result"]["in_transaction"], true, "{begin}");
    q(&mut p, "INSERT INTO live_q3 VALUES (1)", Some("tab-1"));
    // The same tab sees its own uncommitted row on a later run: only possible
    // because the connection stayed pinned. (A second connection reading the
    // table would block on the open write, so it is not asserted here.)
    let own = q(&mut p, "SELECT count(*) FROM live_q3", Some("tab-1"));
    assert_eq!(own["result"]["result"]["rows"], json!([[1]]), "{own}");
    assert_eq!(own["result"]["in_transaction"], true);

    let commit = q(&mut p, "COMMIT", Some("tab-1"));
    assert_eq!(commit["result"]["in_transaction"], false);
    let after = q(&mut p, "SELECT count(*) FROM live_q3", None);
    assert_eq!(after["result"]["rows"], json!([[1]]));
    p.query("DROP TABLE live_q3");
}

#[test]
fn a_failed_statement_leaves_the_transaction_aborted_until_rollback() {
    let Some(mut p) = Plugin::start() else { return };
    let q = |p: &mut Plugin, sql: &str| {
        p.call(
            "execute_query",
            json!({"query": sql, "session_id": "tab-err"}),
        )
    };
    assert_eq!(q(&mut p, "BEGIN")["result"]["in_transaction"], true);
    let bad = q(&mut p, "SELECT 1/0");
    assert!(
        bad["result"]["error"].as_str().unwrap().contains("22012"),
        "{bad}"
    );
    assert_eq!(
        bad["result"]["in_transaction"], true,
        "an aborted transaction is still open: {bad}"
    );
    // Not retried and not hidden: the next statement says the transaction is aborted.
    let next = q(&mut p, "SELECT 1");
    assert!(
        next["result"]["error"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("abort"),
        "{next}"
    );
    let rb = q(&mut p, "ROLLBACK");
    assert_eq!(rb["result"]["in_transaction"], false, "{rb}");
    assert_eq!(
        q(&mut p, "SELECT 1")["result"]["result"]["rows"],
        json!([[1]])
    );
}

#[test]
fn release_session_rolls_back_and_frees_the_connection() {
    let Some(mut p) = Plugin::start() else { return };
    p.query("DROP TABLE IF EXISTS live_q4");
    p.query("CREATE TABLE live_q4 (id INT8 PRIMARY KEY)");
    p.call(
        "execute_query",
        json!({"query": "BEGIN", "session_id": "tab-rel"}),
    );
    p.call(
        "execute_query",
        json!({"query": "INSERT INTO live_q4 VALUES (1)", "session_id": "tab-rel"}),
    );
    let rel = p.call("release_session", json!({"session_id": "tab-rel"}));
    assert_eq!(rel["result"]["released"], true);
    let c = p.query("SELECT count(*) FROM live_q4");
    assert_eq!(
        c["result"]["rows"],
        json!([[0]]),
        "released session must roll back"
    );
    let missing = p.call("release_session", json!({}));
    assert_eq!(missing["error"]["code"], -32602);
    p.query("DROP TABLE live_q4");
}

#[test]
fn an_unpinned_run_never_leaves_a_transaction_for_the_next_borrower() {
    let Some(mut p) = Plugin::start() else { return };
    p.query("DROP TABLE IF EXISTS live_q5");
    p.query("CREATE TABLE live_q5 (id INT8 PRIMARY KEY)");
    // No session_id: a transaction left open is rolled back.
    let r = p.call(
        "execute_query_batch",
        json!({"queries": ["BEGIN", "INSERT INTO live_q5 VALUES (1)"]}),
    );
    assert!(r["result"].is_array());
    let c = p.query("SELECT count(*) FROM live_q5");
    assert_eq!(c["result"]["rows"], json!([[0]]));
    p.query("DROP TABLE live_q5");
}

#[test]
fn schema_is_applied_per_run_and_reset_before_the_connection_is_reused() {
    let Some(mut p) = Plugin::start() else { return };
    seed();
    let r = p.call(
        "execute_query",
        json!({"query": "SELECT count(*) FROM users", "schema": SCHEMA}),
    );
    assert!(
        r.get("error").is_none(),
        "table resolved through the schema: {r}"
    );
    let path = p.query("SHOW search_path");
    assert!(
        !path["result"]["rows"][0][0]
            .as_str()
            .unwrap()
            .contains(SCHEMA),
        "{path}"
    );
    // A user SET does not leak either, and the application name survives the reset.
    p.query("SET application_name = 'leaky'");
    let app = p.query("SHOW application_name");
    assert_eq!(app["result"]["rows"], json!([["tabularis"]]), "{app}");
}

#[test]
fn cancel_stops_a_running_query_and_gets_no_reply_of_its_own() {
    let Some(mut p) = Plugin::start() else { return };
    let slow = p.send("execute_query", json!({"query": "SELECT pg_sleep(60)"}));
    std::thread::sleep(Duration::from_millis(700));

    // The reader keeps reading while the query runs: a ping is answered at once.
    let ping = p.send("ping", json!({}));
    let r = p.recv(5).expect("ping must not wait for the slow query");
    assert_eq!(r["id"], ping, "{r}");

    let started = std::time::Instant::now();
    writeln!(
        p.stdin,
        "{}",
        json!({"jsonrpc": "2.0", "method": "cancel", "params": {"id": slow}})
    )
    .unwrap();
    let r = p.recv(10).expect("cancelled query must answer");
    assert_eq!(r["id"], slow, "{r}");
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("cancel"),
        "{r}"
    );
    assert!(started.elapsed() < Duration::from_secs(8));

    // The notification itself produced no line: the next line is the next reply.
    let next = p.send("ping", json!({}));
    assert_eq!(p.recv(5).unwrap()["id"], next);
    // Cancelling an id that is not running is harmless.
    writeln!(
        p.stdin,
        "{}",
        json!({"jsonrpc": "2.0", "method": "cancel", "params": {"id": 99999}})
    )
    .unwrap();
    let again = p.send("ping", json!({}));
    assert_eq!(p.recv(5).unwrap()["id"], again);
}

// ---- Regression: a host display limit must never change what a write does --

#[test]
fn a_cte_insert_writes_every_row_whatever_the_host_limit() {
    let Some(mut p) = Plugin::start() else { return };
    p.query("DROP TABLE IF EXISTS live_cte1");
    p.query("CREATE TABLE live_cte1 (n INT8)");
    let insert =
        "WITH src AS (SELECT generate_series(1,5) AS n) INSERT INTO live_cte1 SELECT n FROM src";

    // The reported case: limit 2 used to become `LIMIT 3` on the INSERT's source.
    let r = p.call(
        "execute_query",
        json!({"query": insert, "limit": 2, "page": 1}),
    );
    assert!(r.get("error").is_none(), "{r}");
    assert_eq!(r["result"]["affected_rows"], 5, "{r}");
    let c = p.query("SELECT count(*) FROM live_cte1");
    assert_eq!(
        c["result"]["rows"],
        json!([[5]]),
        "a display limit must not truncate a write: {c}"
    );

    // And with no limit, as before.
    p.query("TRUNCATE live_cte1");
    let r = p.call("execute_query", json!({"query": insert}));
    assert_eq!(r["result"]["affected_rows"], 5);
    p.query("DROP TABLE live_cte1");
}

#[test]
fn cte_writes_with_returning_write_everything_and_only_trim_the_display() {
    let Some(mut p) = Plugin::start() else { return };
    p.query("DROP TABLE IF EXISTS live_cte2");
    p.query("CREATE TABLE live_cte2 (n INT8)");
    let r = p.call(
        "execute_query",
        json!({"query": "WITH src AS (SELECT generate_series(1,5) AS n) INSERT INTO live_cte2 SELECT n FROM src RETURNING n", "limit": 2, "page": 1}),
    );
    assert!(r.get("error").is_none(), "{r}");
    assert_eq!(
        r["result"]["rows"].as_array().unwrap().len(),
        2,
        "display capped: {r}"
    );
    assert_eq!(r["result"]["truncated"], true);
    assert!(
        r["result"]["pagination"].is_null(),
        "no pagination on a write: {r}"
    );
    let c = p.query("SELECT count(*) FROM live_cte2");
    assert_eq!(
        c["result"]["rows"],
        json!([[5]]),
        "all rows were still written: {c}"
    );
    p.query("DROP TABLE live_cte2");
}

#[test]
fn cte_update_and_delete_touch_every_row_whatever_the_host_limit() {
    let Some(mut p) = Plugin::start() else { return };
    p.query("DROP TABLE IF EXISTS live_cte3");
    p.query("CREATE TABLE live_cte3 (n INT8, flag BOOL DEFAULT false)");
    p.query("INSERT INTO live_cte3 (n) SELECT generate_series(1,6)");
    let upd = p.call(
        "execute_query",
        json!({"query": "WITH ids AS (SELECT n FROM live_cte3) UPDATE live_cte3 SET flag = true WHERE n IN (SELECT n FROM ids)", "limit": 2, "page": 1}),
    );
    assert_eq!(upd["result"]["affected_rows"], 6, "{upd}");
    let flagged = p.query("SELECT count(*) FROM live_cte3 WHERE flag");
    assert_eq!(flagged["result"]["rows"], json!([[6]]));

    let del = p.call(
        "execute_query",
        json!({"query": "WITH ids AS (SELECT n FROM live_cte3) DELETE FROM live_cte3 WHERE n IN (SELECT n FROM ids)", "limit": 2, "page": 1}),
    );
    assert_eq!(del["result"]["affected_rows"], 6, "{del}");
    let left = p.query("SELECT count(*) FROM live_cte3");
    assert_eq!(left["result"]["rows"], json!([[0]]));
    p.query("DROP TABLE live_cte3");
}

#[test]
fn a_data_modifying_cte_feeding_a_select_still_runs_to_completion() {
    let Some(mut p) = Plugin::start() else { return };
    p.query("DROP TABLE IF EXISTS live_cte4");
    p.query("CREATE TABLE live_cte4 (n INT8)");
    // Pagination may trim the outer SELECT, but the INSERT inside the CTE writes everything.
    let r = p.call(
        "execute_query",
        json!({"query": "WITH ins AS (INSERT INTO live_cte4 SELECT generate_series(1,5) RETURNING n) SELECT n FROM ins ORDER BY n", "limit": 2, "page": 1}),
    );
    assert!(r.get("error").is_none(), "{r}");
    assert_eq!(r["result"]["rows"].as_array().unwrap().len(), 2);
    assert_eq!(r["result"]["pagination"]["has_more"], true);
    let c = p.query("SELECT count(*) FROM live_cte4");
    assert_eq!(c["result"]["rows"], json!([[5]]), "{c}");
    p.query("DROP TABLE live_cte4");
}

#[test]
fn a_read_only_cte_is_still_paginated() {
    let Some(mut p) = Plugin::start() else { return };
    let q = "WITH src AS (SELECT generate_series(1,25) AS n) SELECT n FROM src ORDER BY n";
    let page = |p: &mut Plugin, n: u64| {
        p.call("execute_query", json!({"query": q, "limit": 10, "page": n}))["result"].clone()
    };
    let first = page(&mut p, 1);
    assert_eq!(first["rows"].as_array().unwrap().len(), 10);
    assert_eq!(first["pagination"]["has_more"], true);
    let last = page(&mut p, 3);
    assert_eq!(last["rows"], json!([[21], [22], [23], [24], [25]]));
    assert_eq!(last["pagination"]["has_more"], false);
}

// ---- Regression: session state must not leak to the next borrower ----------
//
// `poolMaxSize: 1` makes the reproduction deterministic: every run, from any
// tab, gets the same physical connection.

fn tab(p: &mut Plugin, session: &str, sql: &str) -> Value {
    p.call(
        "execute_query",
        json!({"query": sql, "schema": "public", "session_id": session}),
    )
}

fn app_name(p: &mut Plugin, session: &str) -> Value {
    let r = tab(p, session, "SHOW application_name");
    r["result"]["result"]["rows"][0][0].clone()
}

#[test]
fn set_in_a_transaction_does_not_outlive_commit_across_separate_calls() {
    let Some(mut p) = Plugin::start_with(json!({"poolMaxSize": 1})) else {
        return;
    };
    assert_eq!(app_name(&mut p, "tab-base"), "tabularis", "baseline");

    assert_eq!(
        tab(&mut p, "tab-a", "BEGIN")["result"]["in_transaction"],
        true
    );
    tab(
        &mut p,
        "tab-a",
        "SET application_name = 'repro-leaked-state'",
    );
    // The setting is visible to its own transaction...
    assert_eq!(app_name(&mut p, "tab-a"), "repro-leaked-state");
    let commit = tab(&mut p, "tab-a", "COMMIT");
    assert_eq!(commit["result"]["in_transaction"], false, "{commit}");

    // ...and gone before another tab borrows the connection.
    assert_eq!(
        app_name(&mut p, "tab-b"),
        "tabularis",
        "state leaked to the next tab"
    );
}

#[test]
fn set_in_a_transaction_does_not_outlive_rollback_across_separate_calls() {
    let Some(mut p) = Plugin::start_with(json!({"poolMaxSize": 1})) else {
        return;
    };
    tab(&mut p, "tab-a", "BEGIN");
    tab(
        &mut p,
        "tab-a",
        "SET application_name = 'leaked-by-rollback-tab'",
    );
    let rb = tab(&mut p, "tab-a", "ROLLBACK");
    assert_eq!(rb["result"]["in_transaction"], false);
    assert_eq!(app_name(&mut p, "tab-b"), "tabularis");
}

#[test]
fn set_does_not_outlive_release_session() {
    let Some(mut p) = Plugin::start_with(json!({"poolMaxSize": 1})) else {
        return;
    };
    tab(&mut p, "tab-a", "BEGIN");
    tab(
        &mut p,
        "tab-a",
        "SET application_name = 'leaked-by-release'",
    );
    let rel = p.call("release_session", json!({"session_id": "tab-a"}));
    assert_eq!(rel["result"]["released"], true);
    assert_eq!(app_name(&mut p, "tab-b"), "tabularis");
}

#[test]
fn set_left_in_an_unpinned_open_transaction_is_cleaned_up() {
    let Some(mut p) = Plugin::start_with(json!({"poolMaxSize": 1})) else {
        return;
    };
    // No session_id: the open transaction is rolled back, and nothing leaks.
    let r = p.call(
        "execute_query_batch",
        json!({"queries": ["BEGIN", "SET application_name = 'leaked-unpinned'"], "schema": "public"}),
    );
    assert!(r["result"].is_array());
    assert_eq!(app_name(&mut p, "tab-b"), "tabularis");
}

#[test]
fn a_set_in_a_run_with_no_transaction_does_not_leak_either() {
    let Some(mut p) = Plugin::start_with(json!({"poolMaxSize": 1})) else {
        return;
    };
    tab(
        &mut p,
        "tab-a",
        "SET application_name = 'leaked-autocommit'",
    );
    assert_eq!(app_name(&mut p, "tab-b"), "tabularis");
}

#[test]
fn set_config_does_not_leak() {
    let Some(mut p) = Plugin::start_with(json!({"poolMaxSize": 1})) else {
        return;
    };
    tab(
        &mut p,
        "tab-a",
        "SELECT set_config('application_name', 'leaked-set-config', false)",
    );
    assert_eq!(app_name(&mut p, "tab-b"), "tabularis");
}

#[test]
fn a_session_keeps_its_settings_for_the_whole_transaction() {
    let Some(mut p) = Plugin::start_with(json!({"poolMaxSize": 1})) else {
        return;
    };
    tab(&mut p, "tab-a", "BEGIN");
    tab(&mut p, "tab-a", "SET application_name = 'mine'");
    // Several more runs inside the same transaction keep seeing it.
    for _ in 0..3 {
        assert_eq!(app_name(&mut p, "tab-a"), "mine");
    }
    tab(&mut p, "tab-a", "COMMIT");
    assert_eq!(app_name(&mut p, "tab-b"), "tabularis");
}

#[test]
fn set_config_does_not_leak_when_no_schema_masks_it() {
    // With a schema, the per-run search_path cleanup would hide a missed
    // `set_config`; without one, only the statement classifier catches it.
    let Some(mut p) = Plugin::start_with(json!({"poolMaxSize": 1})) else {
        return;
    };
    p.call(
        "execute_query",
        json!({"query": "SELECT set_config('application_name', 'leaked-bare', false)"}),
    );
    let r = p.call("execute_query", json!({"query": "SHOW application_name"}));
    assert_eq!(r["result"]["rows"][0][0], "tabularis", "{r}");
}

// ---- Regression: pagination must understand comments, strings and clauses --

/// The first column of every row, or the error text.
fn page_of(
    p: &mut Plugin,
    query: &str,
    limit: u64,
    page: u64,
) -> Result<(Vec<Value>, bool), String> {
    let r = p.call(
        "execute_query",
        json!({"query": query, "limit": limit, "page": page, "schema": "public"}),
    );
    if let Some(e) = r.get("error") {
        return Err(e["message"].as_str().unwrap().to_string());
    }
    let res = &r["result"];
    let rows = res["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row[0].clone())
        .collect();
    Ok((rows, res["pagination"]["has_more"] == true))
}

fn ints(from: i64, to: i64) -> Vec<Value> {
    (from..=to).map(|n| json!(n)).collect()
}

#[test]
fn a_trailing_line_comment_does_not_make_page_two_repeat_page_one() {
    let Some(mut p) = Plugin::start() else { return };
    // The reported case: no newline after the comment.
    let q = "SELECT generate_series(1,25) AS n ORDER BY 1 -- note";
    assert_eq!(page_of(&mut p, q, 10, 1).unwrap(), (ints(1, 10), true));
    assert_eq!(
        page_of(&mut p, q, 10, 2).unwrap(),
        (ints(11, 20), true),
        "page 2 must advance"
    );
    assert_eq!(page_of(&mut p, q, 10, 3).unwrap(), (ints(21, 25), false));
}

#[test]
fn a_block_comment_after_a_user_limit_is_accepted_and_the_limit_still_caps() {
    let Some(mut p) = Plugin::start() else { return };
    // The reported case: used to fail with SQLSTATE 42601.
    let q = "SELECT generate_series(1,25) AS n ORDER BY 1 LIMIT 2 /* note */";
    assert_eq!(page_of(&mut p, q, 10, 1).unwrap(), (ints(1, 2), false));
    // The user's LIMIT caps the total across pages.
    let q = "SELECT generate_series(1,25) AS n ORDER BY 1 LIMIT 12 /* note */";
    assert_eq!(page_of(&mut p, q, 10, 1).unwrap(), (ints(1, 10), true));
    assert_eq!(page_of(&mut p, q, 10, 2).unwrap(), (ints(11, 12), false));
}

#[test]
fn comments_with_a_limit_and_offset_in_every_position() {
    let Some(mut p) = Plugin::start() else { return };
    let base = "SELECT generate_series(1,25) AS n ORDER BY 1";
    let cases = [
        format!("{base} LIMIT 5 -- cap"),
        format!("{base} LIMIT 5 /* cap */"),
        format!("{base} LIMIT /* c */ 5"),
        format!("{base} /* a */ LIMIT 5 /* b */"),
        format!("{base} LIMIT 5 -- cap\n"),
        format!("{base} LIMIT 5; -- cap"),
    ];
    for q in &cases {
        assert_eq!(
            page_of(&mut p, q, 10, 1).unwrap(),
            (ints(1, 5), false),
            "{q}"
        );
    }
    let q = format!("{base} LIMIT 5 OFFSET 3 /* skip */");
    assert_eq!(page_of(&mut p, &q, 10, 1).unwrap(), (ints(4, 8), false));
    // A trailing comment with a user OFFSET, page 2.
    let q = format!("{base} OFFSET 3 -- skip");
    assert_eq!(page_of(&mut p, &q, 10, 2).unwrap(), (ints(14, 23), true));
}

#[test]
fn strings_and_quoted_identifiers_containing_limit_are_preserved() {
    let Some(mut p) = Plugin::start() else { return };
    let q = "SELECT 'LIMIT 5 -- not a clause' AS \"limit\", generate_series(1,25) AS n ORDER BY 2";
    let r = p.call("execute_query", json!({"query": q, "limit": 10, "page": 2}));
    assert!(r.get("error").is_none(), "{r}");
    let rows = r["result"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 10);
    assert_eq!(rows[0], json!(["LIMIT 5 -- not a clause", 11]));
    assert_eq!(r["result"]["columns"][0], "limit");
}

#[test]
fn clauses_the_scanner_cannot_read_fall_back_to_wrapping_and_still_page() {
    let Some(mut p) = Plugin::start() else { return };
    for tail in ["LIMIT ALL", "FETCH FIRST 100 ROWS ONLY", "OFFSET 0 ROWS"] {
        let q = format!("SELECT generate_series(1,25) AS n ORDER BY 1 {tail}");
        assert_eq!(
            page_of(&mut p, &q, 10, 1).unwrap(),
            (ints(1, 10), true),
            "{tail}"
        );
        assert_eq!(
            page_of(&mut p, &q, 10, 2).unwrap(),
            (ints(11, 20), true),
            "page 2 must advance: {tail}"
        );
        assert_eq!(
            page_of(&mut p, &q, 10, 3).unwrap(),
            (ints(21, 25), false),
            "{tail}"
        );
    }
    // A trailing comment on a wrapped statement cannot reach the outer clause.
    let q = "SELECT generate_series(1,25) AS n ORDER BY 1 LIMIT ALL -- note";
    assert_eq!(page_of(&mut p, q, 10, 2).unwrap(), (ints(11, 20), true));
}

#[test]
fn a_limit_inside_a_subquery_is_not_the_users_limit() {
    let Some(mut p) = Plugin::start() else { return };
    let q = "SELECT n FROM (SELECT generate_series(1,25) AS n LIMIT 15) AS s ORDER BY n";
    assert_eq!(page_of(&mut p, q, 10, 2).unwrap(), (ints(11, 15), false));
}

#[test]
fn a_comment_only_or_broken_statement_is_not_rewritten() {
    let Some(mut p) = Plugin::start() else { return };
    // An unterminated comment must still be an error, not silently "fixed".
    let err = page_of(&mut p, "SELECT 1 /* never closed", 10, 1).unwrap_err();
    assert!(
        err.to_lowercase().contains("comment") || err.contains("SQLSTATE"),
        "{err}"
    );
}

// ---- Phase 4: row editing (insert_record, update_record, delete_record) ----

impl Plugin {
    /// A call whose connection object is replaced (e.g. to add a connection URI).
    fn call_with_connection(&mut self, method: &str, connection: Value, extra: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let mut params = json!({ "params": connection });
        for (k, v) in extra.as_object().unwrap() {
            params[k] = v.clone();
        }
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{req}").unwrap();
        let r = self.recv(20).expect("no response");
        assert_eq!(r["id"], id, "{r}");
        r
    }
}

fn fresh_table(p: &mut Plugin, name: &str, ddl: &str) {
    p.query(&format!("DROP TABLE IF EXISTS {name}"));
    let r = p.query(&format!("CREATE TABLE {name} ({ddl})"));
    assert!(r.get("error").is_none(), "{r}");
}

fn rows_of(p: &mut Plugin, sql: &str) -> Vec<Value> {
    let r = p.query(sql);
    assert!(r.get("error").is_none(), "{sql}: {r}");
    r["result"]["rows"].as_array().unwrap().clone()
}

fn count(p: &mut Plugin, table: &str) -> i64 {
    rows_of(p, &format!("SELECT count(*) FROM {table}"))[0][0]
        .as_i64()
        .unwrap()
}

#[test]
fn crud_round_trip_with_a_big_int8_key_sent_as_a_string() {
    let Some(mut p) = Plugin::start() else { return };
    fresh_table(
        &mut p,
        "live_c1",
        "id INT8 PRIMARY KEY DEFAULT unique_rowid(), name STRING, price DECIMAL(10,2), active BOOL",
    );

    // Columns left out get their server default (unique_rowid()).
    let r = p.call(
        "insert_record",
        json!({"table": "live_c1", "data": {"name": "Ann", "price": "12.50", "active": true}}),
    );
    assert_eq!(r["result"], 1, "the reply is the affected-row count: {r}");
    let row = &rows_of(&mut p, "SELECT id, name, price, active FROM live_c1")[0];
    let id = row[0]
        .as_str()
        .expect("a unique_rowid() key is above 2^53, so it travels as a string");
    assert_eq!(
        row.as_array().unwrap()[1..],
        [json!("Ann"), json!("12.50"), json!(true)]
    );

    // The key goes back as the same string and finds the row exactly.
    let r = p.call(
        "update_record",
        json!({"table": "live_c1", "pk_map": {"id": id}, "col_name": "name", "new_val": "Bob"}),
    );
    assert_eq!(r["result"], 1, "{r}");
    assert_eq!(rows_of(&mut p, "SELECT name FROM live_c1")[0][0], "Bob");

    let r = p.call(
        "delete_record",
        json!({"table": "live_c1", "pk_map": {"id": id}}),
    );
    assert_eq!(r["result"], 1, "{r}");
    assert_eq!(count(&mut p, "live_c1"), 0);
    p.query("DROP TABLE live_c1");
}

#[test]
fn updating_a_row_that_no_longer_exists_is_a_conflict_not_success() {
    let Some(mut p) = Plugin::start() else { return };
    fresh_table(&mut p, "live_c2", "id INT8 PRIMARY KEY, name STRING");
    p.query("INSERT INTO live_c2 VALUES (1, 'a')");
    let r = p.call(
        "update_record",
        json!({"table": "live_c2", "pk_map": {"id": 999}, "col_name": "name", "new_val": "x"}),
    );
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("row not found"),
        "{r}"
    );
    // Deleting an already-deleted row is just 0 affected rows.
    let r = p.call(
        "delete_record",
        json!({"table": "live_c2", "pk_map": {"id": 999}}),
    );
    assert_eq!(r["result"], 0, "{r}");
    assert_eq!(count(&mut p, "live_c2"), 1);
    p.query("DROP TABLE live_c2");
}

#[test]
fn composite_keys_and_null_key_values() {
    let Some(mut p) = Plugin::start() else { return };
    fresh_table(
        &mut p,
        "live_c3",
        "a INT8, b STRING, v STRING, PRIMARY KEY (a, b)",
    );
    p.query("INSERT INTO live_c3 VALUES (1,'x','one-x'), (1,'y','one-y'), (2,'x','two-x')");
    // Only the row matching BOTH key columns changes.
    let r = p.call("update_record", json!({"table": "live_c3", "pk_map": {"a": 1, "b": "y"}, "col_name": "v", "new_val": "changed"}));
    assert_eq!(r["result"], 1, "{r}");
    let all = rows_of(&mut p, "SELECT a, b, v FROM live_c3 ORDER BY a, b");
    assert_eq!(
        all,
        [
            json!([1, "x", "one-x"]),
            json!([1, "y", "changed"]),
            json!([2, "x", "two-x"])
        ]
    );
    let r = p.call(
        "delete_record",
        json!({"table": "live_c3", "pk_map": {"b": "x", "a": 2}}),
    );
    assert_eq!(r["result"], 1);
    assert_eq!(count(&mut p, "live_c3"), 2);
    p.query("DROP TABLE live_c3");

    // A NULL key value matches with IS NULL, not `= NULL`.
    fresh_table(&mut p, "live_c3n", "k STRING, v STRING");
    p.query("INSERT INTO live_c3n VALUES (NULL, 'n'), ('a', 'a')");
    let r = p.call(
        "update_record",
        json!({"table": "live_c3n", "pk_map": {"k": null}, "col_name": "v", "new_val": "was-null"}),
    );
    assert_eq!(r["result"], 1, "{r}");
    assert_eq!(
        rows_of(&mut p, "SELECT v FROM live_c3n WHERE k IS NULL")[0][0],
        "was-null"
    );
    p.query("DROP TABLE live_c3n");
}

#[test]
fn a_table_without_a_primary_key_is_edited_through_the_hidden_rowid() {
    let Some(mut p) = Plugin::start() else { return };
    fresh_table(&mut p, "live_c4", "name STRING, qty INT8");
    p.call(
        "insert_record",
        json!({"table": "live_c4", "data": {"name": "a", "qty": 1}}),
    );
    p.call(
        "insert_record",
        json!({"table": "live_c4", "data": {"name": "b", "qty": 2}}),
    );
    let rows = rows_of(&mut p, "SELECT rowid, name FROM live_c4 ORDER BY name");
    let rowid = rows[0][0].clone(); // `rowid` is above 2^53: a string
    assert!(rowid.is_string(), "{rowid}");
    let r = p.call(
        "update_record",
        json!({"table": "live_c4", "pk_map": {"rowid": rowid}, "col_name": "qty", "new_val": 99}),
    );
    assert_eq!(r["result"], 1, "{r}");
    assert_eq!(
        rows_of(&mut p, "SELECT name, qty FROM live_c4 ORDER BY name"),
        [json!(["a", 99]), json!(["b", 2])]
    );
    p.query("DROP TABLE live_c4");
}

#[test]
fn a_key_that_matches_several_rows_changes_nothing() {
    let Some(mut p) = Plugin::start() else { return };
    fresh_table(&mut p, "live_c5", "tag STRING, v STRING");
    p.query("INSERT INTO live_c5 VALUES ('dup','a'), ('dup','b'), ('solo','c')");

    let r = p.call(
        "update_record",
        json!({"table": "live_c5", "pk_map": {"tag": "dup"}, "col_name": "v", "new_val": "X"}),
    );
    let msg = r["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("refusing to change 2 rows") && msg.contains("Nothing was changed"),
        "{r}"
    );
    assert_eq!(
        rows_of(&mut p, "SELECT count(*) FROM live_c5 WHERE v = 'X'")[0][0],
        0,
        "the update must be rolled back"
    );

    let r = p.call(
        "delete_record",
        json!({"table": "live_c5", "pk_map": {"tag": "dup"}}),
    );
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("refusing to change 2 rows"),
        "{r}"
    );
    assert_eq!(
        count(&mut p, "live_c5"),
        3,
        "the delete must be rolled back"
    );
    p.query("DROP TABLE live_c5");
}

#[test]
fn an_empty_row_key_can_never_touch_every_row() {
    let Some(mut p) = Plugin::start() else { return };
    fresh_table(&mut p, "live_c6", "id INT8 PRIMARY KEY, v STRING");
    p.query("INSERT INTO live_c6 VALUES (1,'a'), (2,'b')");
    for extra in [json!({"pk_map": {}}), json!({})] {
        let mut upd = json!({"table": "live_c6", "col_name": "v", "new_val": "X"});
        let mut del = json!({"table": "live_c6"});
        for (k, v) in extra.as_object().unwrap() {
            upd[k] = v.clone();
            del[k] = v.clone();
        }
        let u = p.call("update_record", upd);
        assert_eq!(u["error"]["code"], -32602, "{u}");
        let d = p.call("delete_record", del);
        assert_eq!(d["error"]["code"], -32602, "{d}");
    }
    assert_eq!(count(&mut p, "live_c6"), 2);
    assert_eq!(
        rows_of(&mut p, "SELECT count(*) FROM live_c6 WHERE v = 'X'")[0][0],
        0
    );
    p.query("DROP TABLE live_c6");
}

#[test]
fn unknown_tables_and_columns_are_reported_not_guessed() {
    let Some(mut p) = Plugin::start() else { return };
    fresh_table(&mut p, "live_c7", "id INT8 PRIMARY KEY, v STRING");
    let r = p.call(
        "insert_record",
        json!({"table": "live_no_such_table", "data": {"a": 1}}),
    );
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("was not found"),
        "{r}"
    );
    let r = p.call(
        "insert_record",
        json!({"table": "live_c7", "data": {"nope": 1}}),
    );
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("column \"nope\" does not exist"),
        "{r}"
    );
    let r = p.call(
        "update_record",
        json!({"table": "live_c7", "pk_map": {"idd": 1}, "col_name": "v", "new_val": "x"}),
    );
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("column \"idd\" does not exist"),
        "{r}"
    );
    let r = p.call("insert_record", json!({}));
    assert_eq!(r["error"]["code"], -32602, "{r}");
    p.query("DROP TABLE live_c7");
}

#[test]
fn every_value_type_survives_an_insert_and_a_read_back() {
    let Some(mut p) = Plugin::start() else { return };
    p.query("DROP TYPE IF EXISTS live_c8_mood");
    p.query("CREATE TYPE live_c8_mood AS ENUM ('sad', 'happy')");
    fresh_table(
        &mut p,
        "live_c8",
        "id INT8 PRIMARY KEY, i2 INT2, d DECIMAL(30,3), f FLOAT8, b BOOL, s STRING, y BYTES, u UUID, j JSONB, \
         ts TIMESTAMPTZ, iv INTERVAL, ip INET, a INT8[], sa STRING[], m live_c8_mood",
    );
    let blob = "BLOB:3:application/octet-stream:AAH/"; // bytes 0x00 0x01 0xFF
    let r = p.call(
        "insert_record",
        json!({"table": "live_c8", "data": {
            "id": "1152921504606846977", "i2": 7, "d": "123456789012345678901234567.890", "f": "Infinity", "b": "t",
            "s": "it's \"quoted\"", "y": blob, "u": "DEADBEEF-0001-0203-0405-060708090A0B", "j": {"k": [1, 2]},
            "ts": "2024-03-05T12:30:45.5Z", "iv": "1 day 02:00:00", "ip": "10.0.0.0/24",
            "a": [1, null, 3], "sa": ["a b", "q\"x", "b\\s"], "m": "happy"}}),
    );
    assert_eq!(r["result"], 1, "{r}");
    let row = &rows_of(&mut p, "SELECT * FROM live_c8")[0];
    assert_eq!(row[0], "1152921504606846977");
    assert_eq!(row[1], 7);
    assert_eq!(row[2], "123456789012345678901234567.890");
    assert_eq!(row[3], "Infinity");
    assert_eq!(row[4], true);
    assert_eq!(row[5], "it's \"quoted\"");
    assert!(row[6].as_str().unwrap().starts_with("BLOB:3:"));
    assert_eq!(row[7], "deadbeef-0001-0203-0405-060708090a0b");
    assert_eq!(row[8], json!({"k": [1, 2]}));
    assert_eq!(row[9], "2024-03-05T12:30:45.5Z");
    assert_eq!(row[10], "1 day 02:00:00");
    assert_eq!(row[11], "10.0.0.0/24");
    assert_eq!(row[12], json!([1, null, 3]));
    assert_eq!(row[13], json!(["a b", "q\"x", "b\\s"]));
    assert_eq!(row[14], "happy");

    // The same types through update_record, keyed by the exact string key.
    let key = json!({"id": "1152921504606846977"});
    for (col, val, expect) in [
        ("d", json!("0.001"), json!("0.001")),
        ("m", json!("sad"), json!("sad")),
        ("a", json!([9]), json!([9])),
        ("j", json!("{\"x\": true}"), json!({"x": true})),
        ("b", json!(false), json!(false)),
        ("s", Value::Null, Value::Null),
    ] {
        let r = p.call(
            "update_record",
            json!({"table": "live_c8", "pk_map": key, "col_name": col, "new_val": val}),
        );
        assert_eq!(r["result"], 1, "{col}: {r}");
        assert_eq!(
            rows_of(&mut p, &format!("SELECT {col} FROM live_c8"))[0][0],
            expect,
            "{col}"
        );
    }
    p.query("DROP TABLE live_c8");
    p.query("DROP TYPE live_c8_mood");
}

#[test]
fn the_default_sentinel_resets_a_column_to_its_default() {
    let Some(mut p) = Plugin::start() else { return };
    fresh_table(
        &mut p,
        "live_c9",
        "id INT8 PRIMARY KEY, status STRING DEFAULT 'new'",
    );
    p.query("INSERT INTO live_c9 VALUES (1, 'old')");
    let r = p.call("update_record", json!({"table": "live_c9", "pk_map": {"id": 1}, "col_name": "status", "new_val": "__USE_DEFAULT__"}));
    assert_eq!(r["result"], 1, "{r}");
    assert_eq!(rows_of(&mut p, "SELECT status FROM live_c9")[0][0], "new");
    // An insert with no columns takes every default.
    p.call(
        "insert_record",
        json!({"table": "live_c9", "data": {"id": 2}}),
    );
    assert_eq!(
        rows_of(&mut p, "SELECT status FROM live_c9 WHERE id = 2")[0][0],
        "new"
    );
    p.query("DROP TABLE live_c9");
}

#[test]
fn edits_work_on_a_connection_that_defaults_to_read_only() {
    let Some(mut p) = Plugin::start() else { return };
    fresh_table(&mut p, "live_c10", "id INT8 PRIMARY KEY, v STRING");
    let (host, port) = live_node().unwrap();
    let uri = format!("cockroachdb://root@{host}:{port}/defaultdb?sslmode=disable&options=-c%20default_transaction_read_only%3Don");
    let mut conn = p.conn();
    conn["params"]["connection_uri"] = json!(uri);
    let connection = conn["params"].clone();

    // Proof the setup is real: a plain statement is refused by the server.
    let plain = p.call_with_connection(
        "execute_query",
        connection.clone(),
        json!({"query": "INSERT INTO live_c10 VALUES (1, 'x')"}),
    );
    assert!(
        plain["error"]["message"]
            .as_str()
            .unwrap()
            .contains("25006"),
        "{plain}"
    );

    // A driver-owned `BEGIN READ WRITE` transaction still saves the edit.
    let r = p.call_with_connection(
        "insert_record",
        connection.clone(),
        json!({"table": "live_c10", "data": {"id": 1, "v": "a"}}),
    );
    assert_eq!(r["result"], 1, "{r}");
    let r = p.call_with_connection(
        "update_record",
        connection.clone(),
        json!({"table": "live_c10", "pk_map": {"id": 1}, "col_name": "v", "new_val": "b"}),
    );
    assert_eq!(r["result"], 1, "{r}");
    let r = p.call_with_connection(
        "delete_record",
        connection,
        json!({"table": "live_c10", "pk_map": {"id": 1}}),
    );
    assert_eq!(r["result"], 1, "{r}");
    p.query("DROP TABLE live_c10");
}

#[test]
fn a_failed_write_leaves_the_connection_clean_for_the_next_one() {
    // One connection in the pool: if a failure left a transaction open, the
    // next edit would fail or run inside it.
    let Some(mut p) = Plugin::start_with(json!({"poolMaxSize": 1})) else {
        return;
    };
    fresh_table(&mut p, "live_c11", "id INT8 PRIMARY KEY, v STRING NOT NULL");
    p.call(
        "insert_record",
        json!({"table": "live_c11", "data": {"id": 1, "v": "a"}}),
    );
    let dup = p.call(
        "insert_record",
        json!({"table": "live_c11", "data": {"id": 1, "v": "b"}}),
    );
    assert!(
        dup["error"]["message"].as_str().unwrap().contains("23505"),
        "{dup}"
    );
    let null = p.call(
        "insert_record",
        json!({"table": "live_c11", "data": {"id": 2, "v": null}}),
    );
    assert!(
        null["error"]["message"].as_str().unwrap().contains("23502"),
        "{null}"
    );
    let bad = p.call(
        "insert_record",
        json!({"table": "live_c11", "data": {"id": "not-a-number", "v": "z"}}),
    );
    assert_eq!(bad["error"]["code"], -32602, "{bad}");

    let ok = p.call(
        "insert_record",
        json!({"table": "live_c11", "data": {"id": 3, "v": "c"}}),
    );
    assert_eq!(ok["result"], 1, "{ok}");
    assert_eq!(count(&mut p, "live_c11"), 2);
    p.query("DROP TABLE live_c11");
}

#[test]
fn the_schema_defaults_to_the_current_one_and_can_be_named() {
    let Some(mut p) = Plugin::start() else { return };
    p.query("DROP SCHEMA IF EXISTS live_c12s CASCADE");
    p.query("CREATE SCHEMA live_c12s");
    p.query("CREATE TABLE live_c12s.t (id INT8 PRIMARY KEY, v STRING)");
    let r = p.call(
        "insert_record",
        json!({"table": "t", "schema": "live_c12s", "data": {"id": 1, "v": "in-schema"}}),
    );
    assert_eq!(r["result"], 1, "{r}");
    assert_eq!(
        rows_of(&mut p, "SELECT v FROM live_c12s.t")[0][0],
        "in-schema"
    );
    // Without a schema the table is looked up in `public`, where `t` does not exist.
    let r = p.call("insert_record", json!({"table": "t", "data": {"id": 2}}));
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("was not found"),
        "{r}"
    );
    p.query("DROP SCHEMA live_c12s CASCADE");
}

// ---- BLOBs ----------------------------------------------------------------

fn temp_path(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("live-blob-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn blob_cells_can_be_previewed_and_saved_to_a_file() {
    let Some(mut p) = Plugin::start() else { return };
    fresh_table(
        &mut p,
        "live_b1",
        "id INT8 PRIMARY KEY, data BYTES, label STRING",
    );
    let png = [0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3, 4];
    let b64 = base64_of(&png);
    let wire = format!("BLOB:{}:image/png:{b64}", png.len());
    p.call(
        "insert_record",
        json!({"table": "live_b1", "data": {"id": 1, "data": wire, "label": "pic"}}),
    );
    p.call(
        "insert_record",
        json!({"table": "live_b1", "data": {"id": 2, "label": "no data"}}),
    );

    let r = p.call(
        "fetch_blob_as_data_url",
        json!({"table": "live_b1", "col_name": "data", "pk_map": {"id": 1}}),
    );
    assert_eq!(
        r["result"],
        format!("BLOB:{}:image/png:{b64}", png.len()),
        "{r}"
    );

    let path = temp_path("out.png");
    let r = p.call("save_blob_to_file", json!({"table": "live_b1", "col_name": "data", "pk_map": {"id": 1}, "file_path": path.display().to_string()}));
    assert!(r.get("error").is_none() && r["result"].is_null(), "{r}");
    assert_eq!(std::fs::read(&path).unwrap(), png);

    // Failure modes name the problem.
    let e = |r: &Value| {
        r["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    let null = p.call(
        "fetch_blob_as_data_url",
        json!({"table": "live_b1", "col_name": "data", "pk_map": {"id": 2}}),
    );
    assert!(e(&null).contains("is NULL"), "{null}");
    let missing = p.call(
        "fetch_blob_as_data_url",
        json!({"table": "live_b1", "col_name": "data", "pk_map": {"id": 99}}),
    );
    assert!(e(&missing).contains("row not found"), "{missing}");
    let not_bytes = p.call(
        "fetch_blob_as_data_url",
        json!({"table": "live_b1", "col_name": "label", "pk_map": {"id": 1}}),
    );
    assert!(e(&not_bytes).contains("not BYTES"), "{not_bytes}");
    let no_key = p.call(
        "fetch_blob_as_data_url",
        json!({"table": "live_b1", "col_name": "data", "pk_map": {}}),
    );
    assert_eq!(no_key["error"]["code"], -32602, "{no_key}");
    let bad_path = p.call("save_blob_to_file", json!({"table": "live_b1", "col_name": "data", "pk_map": {"id": 1}, "file_path": "/no/such/dir/x.bin"}));
    assert!(e(&bad_path).contains("does not exist"), "{bad_path}");
    p.query("DROP TABLE live_b1");
    let _ = std::fs::remove_file(path);
}

#[test]
fn a_picked_file_is_stored_as_bytes_and_respects_the_size_cap() {
    let Some(mut p) = Plugin::start() else { return };
    fresh_table(&mut p, "live_b2", "id INT8 PRIMARY KEY, data BYTES");
    let file = temp_path("upload.bin");
    let content: Vec<u8> = (0..=255u8).collect();
    std::fs::write(&file, &content).unwrap();
    let file_ref = format!(
        "BLOB_FILE_REF:{}:application/octet-stream:{}",
        content.len(),
        file.display()
    );

    let r = p.call(
        "insert_record",
        json!({"table": "live_b2", "data": {"id": 1, "data": file_ref}, "max_blob_size": 1024}),
    );
    assert_eq!(r["result"], 1, "{r}");
    let back = p.call(
        "fetch_blob_as_data_url",
        json!({"table": "live_b2", "col_name": "data", "pk_map": {"id": 1}}),
    );
    assert!(
        back["result"].as_str().unwrap().starts_with("BLOB:256:"),
        "{back}"
    );

    // Over the host's cap: refused, and nothing is stored (not even the path as text).
    let r = p.call(
        "insert_record",
        json!({"table": "live_b2", "data": {"id": 2, "data": file_ref}, "max_blob_size": 100}),
    );
    assert!(
        r["error"]["message"].as_str().unwrap().contains("exceeds"),
        "{r}"
    );
    // Update from a picked file too.
    let r = p.call("update_record", json!({"table": "live_b2", "pk_map": {"id": 1}, "col_name": "data", "new_val": file_ref, "max_blob_size": 1024}));
    assert_eq!(r["result"], 1, "{r}");
    assert_eq!(count(&mut p, "live_b2"), 1);
    let missing = p.call(
        "insert_record",
        json!({"table": "live_b2", "data": {"id": 3, "data": "BLOB_FILE_REF:1:x/y:/no/such/file"}}),
    );
    assert!(
        missing["error"]["message"]
            .as_str()
            .unwrap()
            .contains("cannot read"),
        "{missing}"
    );
    assert_eq!(count(&mut p, "live_b2"), 1);
    p.query("DROP TABLE live_b2");
    let _ = std::fs::remove_file(file);
}

/// Standard base64, enough for test data (no dependency in the test crate).
fn base64_of(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n = u32::from(chunk[0]) << 16
            | u32::from(*chunk.get(1).unwrap_or(&0)) << 8
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}
