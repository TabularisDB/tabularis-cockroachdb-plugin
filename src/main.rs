//! Entry point: read JSON-RPC lines from stdin, dispatch, write responses.
//
// Scaffolded utilities (`ConnectionParams`, `quote_identifier`, `paginate`,
// etc.) are unused until you wire them into your handlers. Remove the
// crate-level `allow(dead_code)` once you start using them — the compiler
// will then correctly flag code you forgot to reach.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinSet;

mod binding;
mod cancel;
mod client;
mod error;
mod extract;
mod handlers;
mod introspection;
mod models;
mod retry;
mod rpc;
mod session;
mod settings;
mod sqlkind;
mod sqlscan;
mod tls;
mod utils;
mod version;

/// Requests that run at once. A slow query must not stop other calls from
/// being read, and `cancel` / `ping` skip this limit entirely.
const MAX_CONCURRENT_REQUESTS: usize = 16;

/// How long in-flight requests get to finish after stdin closes (the host is
/// gone) before the process exits anyway.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Idle pinned sessions are swept this often.
const SESSION_SWEEP_INTERVAL: Duration = Duration::from_secs(600);

/// Methods that must never queue behind slow queries: `cancel` is the relief
/// valve for them, and a health check must answer while they run.
fn is_priority(method: Option<&str>) -> bool {
    matches!(
        method,
        Some("cancel" | "ping" | "test_connection" | "initialize" | "shutdown")
    )
}

/// Single writer: every response goes through one task so concurrent handlers
/// never interleave bytes on stdout. Nothing else may write to stdout.
async fn run_writer(mut rx: mpsc::UnboundedReceiver<String>) {
    let mut stdout = tokio::io::stdout();
    while let Some(mut body) = rx.recv().await {
        body.push('\n');
        if stdout.write_all(body.as_bytes()).await.is_err() {
            break;
        }
        let _ = stdout.flush().await;
    }
}

async fn handle(line: String, out: mpsc::UnboundedSender<String>) {
    // `None` is a notification (such as `cancel`): no reply is written.
    let Some(response) = rpc::handle_line(&line).await else {
        return;
    };
    let body = serde_json::to_string(&response).unwrap_or_else(|err| {
        format!(
            "{{\"jsonrpc\":\"2.0\",\"error\":{{\"code\":-32603,\"message\":\"serialization failed: {err}\"}},\"id\":null}}",
        )
    });
    let _ = out.send(body);
}

#[tokio::main]
async fn main() {
    let (out_tx, out_rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(run_writer(out_rx));

    tokio::spawn(async {
        let mut tick = tokio::time::interval(SESSION_SWEEP_INTERVAL);
        loop {
            tick.tick().await;
            session::sweep_idle().await;
        }
    });

    let permits = Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS));
    let mut in_flight: JoinSet<()> = JoinSet::new();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();

    loop {
        // Reap finished handlers so the set does not grow for the process lifetime.
        while in_flight.try_join_next().is_some() {}

        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(err) => {
                eprintln!("stdin read error, exiting: {err}");
                break;
            }
        };
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        // Never block the reader: the permit is awaited inside the task, so a
        // `cancel` that arrives while every slot is busy is still read.
        let out = out_tx.clone();
        if is_priority(rpc::peek_method(&line).as_deref()) {
            in_flight.spawn(handle(line, out));
        } else {
            let permits = permits.clone();
            in_flight.spawn(async move {
                let _permit = permits.acquire_owned().await;
                handle(line, out).await;
            });
        }
    }

    // stdin closed: the host is gone. Give in-flight requests a moment, then
    // roll back pinned transactions and leave.
    let _ = tokio::time::timeout(SHUTDOWN_GRACE, async {
        while in_flight.join_next().await.is_some() {}
    })
    .await;
    in_flight.abort_all();
    session::release_all().await;
    drop(out_tx);
    let _ = tokio::time::timeout(Duration::from_secs(1), writer).await;
}
