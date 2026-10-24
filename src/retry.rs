//! Retry policy for `SQLSTATE 40001` (serialization failure).
//!
//! Under `SERIALIZABLE` contention aborts transactions with `40001`.
//! Only a statement the driver runs alone, outside any transaction, can be
//! retried safely; the user's own `BEGIN ... COMMIT` must see the error.

use std::time::Duration;

pub const SERIALIZATION_FAILURE: &str = "40001";
pub const STATEMENT_COMPLETION_UNKNOWN: &str = "40003";

/// Whether a failed statement may be run again.
///
/// - `40001` only. `40003` (completion unknown) is never retried, because
///   the statement may have committed.
/// - Not inside the user's explicit transaction: it is aborted and only
///   `ROLLBACK` works.
/// - Not after any part of the result was streamed to the host.
pub fn is_retryable(sqlstate: Option<&str>, in_user_txn: bool, streamed: bool) -> bool {
    sqlstate == Some(SERIALIZATION_FAILURE) && !in_user_txn && !streamed
}

/// Backoff before retry number `attempt` (0-based): 10 ms, 20 ms, 40 ms, ...
/// capped at 1 s, with up to 50% jitter taken off. `jitter` is in `0.0..=1.0`.
pub fn backoff_delay(attempt: u32, jitter: f64) -> Duration {
    let base_ms = 10u64.saturating_mul(1u64 << attempt.min(10)).min(1000);
    let reduction = (base_ms as f64 * 0.5 * jitter.clamp(0.0, 1.0)) as u64;
    Duration::from_millis(base_ms - reduction)
}

/// A cheap jitter source (no `rand` dependency): the sub-second clock.
pub fn jitter_from_clock() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    f64::from(nanos % 1000) / 1000.0
}

#[cfg(test)]
#[path = "retry_tests.rs"]
mod tests;
