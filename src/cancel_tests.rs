use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::json;

use super::*;

fn counting(counter: &Arc<AtomicUsize>) -> CancelAction {
    let c = counter.clone();
    Box::new(move || {
        Box::pin(async move {
            c.fetch_add(1, Ordering::SeqCst);
        })
    })
}

fn run<F: Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(f)
}

#[test]
fn keys_cover_numbers_and_strings() {
    assert_eq!(key_for(&json!(5)).as_deref(), Some("5"));
    assert_eq!(key_for(&json!("abc")).as_deref(), Some("abc"));
    assert_eq!(key_for(&Value::Null), None);
}

#[test]
fn cancel_runs_the_registered_action_once() {
    let n = Arc::new(AtomicUsize::new(0));
    let _guard = CancelGuard::with_action(&json!(9001), counting(&n));
    run(cancel(&json!(9001)));
    run(cancel(&json!(9001)));
    assert_eq!(n.load(Ordering::SeqCst), 1, "a repeated cancel is a no-op");
}

#[test]
fn unknown_ids_are_ignored() {
    run(cancel(&json!(424242)));
    run(cancel(&Value::Null));
}

#[test]
fn a_finished_statement_cannot_be_cancelled() {
    let n = Arc::new(AtomicUsize::new(0));
    {
        let _guard = CancelGuard::with_action(&json!(9002), counting(&n));
    }
    run(cancel(&json!(9002)));
    assert_eq!(n.load(Ordering::SeqCst), 0);
}

#[test]
fn numeric_and_string_ids_do_not_collide() {
    let n = Arc::new(AtomicUsize::new(0));
    let _guard = CancelGuard::with_action(&json!(9003), counting(&n));
    run(cancel(&json!("9003x")));
    assert_eq!(n.load(Ordering::SeqCst), 0);
    run(cancel(&json!(9003)));
    assert_eq!(n.load(Ordering::SeqCst), 1);
}
