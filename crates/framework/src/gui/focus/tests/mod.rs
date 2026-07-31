use super::*;

#[test]
fn session_context_rebuilds_only_on_new_content_and_throttled() {
    let interval = SESSION_CTX_MIN_INTERVAL;
    assert!(!session_ctx_due(5, 5, interval * 2), "unchanged content never rebuilds");
    assert!(!session_ctx_due(6, 5, interval / 2), "bursty output is throttled");
    assert!(session_ctx_due(6, 5, interval), "new content past the throttle rebuilds");
}
