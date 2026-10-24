//! Connections pinned to a host session (an editor tab).
//!
//! `execute_query_batch` runs every statement of one batch on a single pooled
//! connection, so `BEGIN ... COMMIT` inside one script works. The workflow a
//! transaction exists for is `BEGIN`, inspect, change, verify, `COMMIT`, each
//! as its own run. Between runs the connection would go back to the pool and
//! the transaction would be stranded.
//!
//! When the host sends a `session_id`, a run that leaves a transaction open
//! keeps its connection here until the session commits, rolls back, closes,
//! or goes idle. The pool recycles with `RecyclingMethod::Fast`, which resets
//! nothing, so a pinned connection is always rolled back before it returns to
//! the pool; otherwise the next borrower would inherit the transaction.
//!
//! Each session has its own lock, held for a whole run, so two overlapping
//! calls for one session run one after the other on the same connection.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use deadpool_postgres::Object;
use tokio::sync::{Mutex, OwnedMutexGuard};

struct Pinned {
    client: Object,
    last_used: Instant,
}

/// A session's pinned connection, if any. Holding the guard serializes runs.
#[derive(Default)]
pub struct Slot(Option<Pinned>);

/// A slot dropped while holding a connection closes it instead of returning
/// it to the pool, where the next borrower would inherit its transaction.
impl Drop for Slot {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            drop(Object::take(p.client));
        }
    }
}

impl Slot {
    /// Take the pinned connection; the caller must pin it again or end its transaction.
    pub fn take(&mut self) -> Option<Object> {
        self.0.take().map(|p| p.client)
    }

    pub fn pin(&mut self, client: Object) {
        self.0 = Some(Pinned {
            client,
            last_used: Instant::now(),
        });
    }
}

/// A pinned connection holds its transaction's locks. A session untouched for
/// this long is rolled back and released by [`sweep_idle`].
pub const MAX_IDLE: Duration = Duration::from_secs(30 * 60);

type SessionMap = HashMap<String, Arc<Mutex<Slot>>>;

fn sessions() -> &'static Mutex<SessionMap> {
    static SESSIONS: OnceLock<Mutex<SessionMap>> = OnceLock::new();
    SESSIONS.get_or_init(Default::default)
}

/// End the transaction before the client goes back to the pool. A failing
/// `ROLLBACK` is only logged: the connection is unusable and the pool will
/// discard it.
pub async fn rollback_and_release(client: Object) {
    if let Err(e) = client.batch_execute("ROLLBACK").await {
        eprintln!("ROLLBACK while releasing a pinned session failed: {e}");
    }
}

/// Lock `session_id`'s slot, waiting for any run already holding it.
pub async fn lock(session_id: &str) -> OwnedMutexGuard<Slot> {
    let slot = sessions()
        .lock()
        .await
        .entry(session_id.to_string())
        .or_default()
        .clone();
    slot.lock_owned().await
}

/// Roll back and release every session idle past [`MAX_IDLE`], and forget
/// slots nothing holds. A slot in use by a run is skipped.
pub async fn sweep_idle() {
    let mut expired = Vec::new();
    {
        let now = Instant::now();
        let mut map = sessions().lock().await;
        map.retain(|_, slot| {
            let Ok(mut guard) = slot.try_lock() else {
                return true;
            };
            if guard
                .0
                .as_ref()
                .is_some_and(|p| now.duration_since(p.last_used) > MAX_IDLE)
            {
                expired.extend(guard.take());
            }
            guard.0.is_some() || Arc::strong_count(slot) > 1
        });
    }
    for client in expired {
        eprintln!(
            "Releasing a pinned session idle for over {} minutes",
            MAX_IDLE.as_secs() / 60
        );
        rollback_and_release(client).await;
    }
}

/// Roll back and release the connection pinned to `session_id`, if any,
/// after any run still in flight for it finishes.
pub async fn release(session_id: &str) {
    let client = lock(session_id).await.take();
    if let Some(client) = client {
        rollback_and_release(client).await;
    }
}

/// Roll back and release every pinned connection so shutdown leaves no
/// transaction open on the server. A session a run still holds is skipped;
/// its connection closes with the process.
pub async fn release_all() {
    let mut clients: Vec<Object> = Vec::new();
    sessions()
        .lock()
        .await
        .retain(|_, slot| match slot.try_lock() {
            Ok(mut guard) => {
                clients.extend(guard.take());
                false
            }
            Err(_) => true,
        });
    for client in clients {
        rollback_and_release(client).await;
    }
}
