use super::*;

#[test]
fn only_serialization_failures_outside_a_user_txn_retry() {
    assert!(is_retryable(Some("40001"), false, false));
    assert!(
        !is_retryable(Some("40001"), true, false),
        "user transaction"
    );
    assert!(
        !is_retryable(Some("40001"), false, true),
        "already streamed"
    );
    assert!(
        !is_retryable(Some("40003"), false, false),
        "completion unknown"
    );
    assert!(!is_retryable(Some("23505"), false, false));
    assert!(!is_retryable(Some("57014"), false, false));
    assert!(!is_retryable(None, false, false));
}

#[test]
fn backoff_doubles_and_caps_at_one_second() {
    let ms = |a| backoff_delay(a, 0.0).as_millis();
    assert_eq!([ms(0), ms(1), ms(2), ms(3)], [10, 20, 40, 80]);
    assert_eq!(ms(6), 640);
    assert_eq!(ms(7), 1000);
    assert_eq!(ms(40), 1000);
}

#[test]
fn jitter_only_shortens_the_delay() {
    for attempt in 0..8 {
        let full = backoff_delay(attempt, 0.0);
        let jittered = backoff_delay(attempt, 1.0);
        assert!(jittered <= full);
        assert!(jittered >= full / 2);
    }
    assert_eq!(
        backoff_delay(2, 5.0),
        backoff_delay(2, 1.0),
        "jitter is clamped"
    );
    assert!((0.0..1.0).contains(&jitter_from_clock()));
}
