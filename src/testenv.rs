//! Test-only: where the live CockroachDB node is.
//!
//! Live tests need a running node. Without `COCKROACH_HOST` they skip, which is
//! convenient on a laptop, but a skipped test is reported as *passed*, so a CI
//! job that forgot to export the variable would go green without testing
//! anything. Set `REQUIRE_LIVE_DB=1` (CI does) to turn a missing host into a
//! failure instead.

/// The node to test against, or `None` to skip. Panics when the host is
/// missing and `REQUIRE_LIVE_DB` is set. An unreachable configured node is not
/// handled here: the test's own connection attempt fails.
pub fn live_node() -> Option<(String, u16)> {
    let host = std::env::var("COCKROACH_HOST")
        .ok()
        .filter(|h| !h.is_empty());
    match host {
        Some(host) => {
            let port = std::env::var("COCKROACH_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(26257);
            Some((host, port))
        }
        None if required() => panic!(
            "REQUIRE_LIVE_DB is set but COCKROACH_HOST is not: refusing to skip a live test \
             (set COCKROACH_HOST, and COCKROACH_PORT if the node is not on 26257)"
        ),
        None => None,
    }
}

fn required() -> bool {
    std::env::var("REQUIRE_LIVE_DB").is_ok_and(|v| !v.is_empty() && v != "0")
}
