//! Live check of the one behavior of `run_in_transaction` that is not visible
//! through the RPC surface: when a later statement fails, an earlier one is
//! rolled back. (A definition that fails to compile is caught by the first
//! `CREATE OR REPLACE VIEW`, before the drop-and-create path is taken, so only a
//! direct call can make `CREATE` fail after `DROP` has succeeded.)

use serde_json::json;

use super::*;

fn connection() -> Option<ConnectionParams> {
    let (host, port) = crate::testenv::live_node()?;
    Some(ConnectionParams::from_value(&json!({
        "host": host, "port": port, "username": "root", "database": "defaultdb", "ssl_mode": "disable"
    })))
}

#[test]
fn a_failure_after_the_drop_brings_the_old_view_back() {
    let Some(conn) = connection() else { return };
    crate::client::block_on(async {
        let entry = client::get_pool(&conn, PoolKind::Query).unwrap();
        let setup = entry.pool.get().await.unwrap();
        setup
            .batch_execute(
                "DROP VIEW IF EXISTS live_tx_v; DROP TABLE IF EXISTS live_tx_t; \
                 CREATE TABLE live_tx_t (id INT8 PRIMARY KEY, name STRING); \
                 INSERT INTO live_tx_t VALUES (1, 'a'); \
                 CREATE VIEW live_tx_v AS SELECT id, name FROM live_tx_t",
            )
            .await
            .unwrap();
        drop(setup);

        // The DROP succeeds, the second statement fails: the view must survive.
        let client = entry.pool.get().await.unwrap();
        let failed = run_in_transaction(
            client,
            &[
                "DROP VIEW live_tx_v".to_string(),
                "SELECT * FROM live_tx_no_such_table".to_string(),
            ],
        )
        .await;
        assert!(failed.is_err());

        let check = entry.pool.get().await.unwrap();
        let rows = check
            .query("SELECT id, name FROM live_tx_v", &[])
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "the DROP must have been rolled back");
        // And the connection that ran the failed transaction is usable again.
        let status = check.simple_query("SHOW TRANSACTION STATUS").await.unwrap();
        assert!(format!("{status:?}").contains("NoTxn"), "{status:?}");

        // The same statements that succeed are applied together.
        let client = entry.pool.get().await.unwrap();
        run_in_transaction(
            client,
            &[
                "DROP VIEW live_tx_v".to_string(),
                "CREATE VIEW live_tx_v AS SELECT name FROM live_tx_t".to_string(),
            ],
        )
        .await
        .unwrap();
        let rows = check.query("SELECT * FROM live_tx_v", &[]).await.unwrap();
        assert_eq!(rows[0].len(), 1, "the view now has one column");
        check
            .batch_execute("DROP VIEW live_tx_v; DROP TABLE live_tx_t")
            .await
            .unwrap();
    });
}
